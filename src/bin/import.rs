use marionette::config::Config;
use marionette::db::{self, Account};
use marionette::import_util;
use serde_json::Value;
use std::env;
use std::path::PathBuf;
use uuid::Uuid;

fn usage() {
    eprintln!(
        "Usage:
  marionette-import --file <path.json> [--provider grok-cli|qoder|blackbox] [--replace] [--db path]
  marionette-import --from-9router <data.sqlite> [--provider grok-cli|qoder|blackbox] [--replace] [--db path]
  marionette-import --from-9router-backup <backup.json> [--replace] [--db path]

Env: MARIONETTE_DB (default ./data/marionette.sqlite)
  --replace  wipe existing supported-provider accounts before importing"
    );
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 || args.iter().any(|a| a == "-h" || a == "--help") {
        usage();
        std::process::exit(if args.len() < 2 { 1 } else { 0 });
    }

    let mut file: Option<PathBuf> = None;
    let mut from_9router: Option<PathBuf> = None;
    let mut from_9router_backup: Option<PathBuf> = None;
    let mut provider = "grok-cli".to_string();
    let mut db_path: Option<PathBuf> = None;
    let mut replace = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                i += 1;
                file = Some(PathBuf::from(&args[i]));
            }
            "--from-9router" => {
                i += 1;
                from_9router = Some(PathBuf::from(&args[i]));
            }
            "--from-9router-backup" => {
                i += 1;
                from_9router_backup = Some(PathBuf::from(&args[i]));
            }
            "--provider" => {
                i += 1;
                provider = args[i].clone();
            }
            "--replace" => {
                replace = true;
            }
            "--db" => {
                i += 1;
                db_path = Some(PathBuf::from(&args[i]));
            }
            other => {
                eprintln!("unknown arg: {other}");
                usage();
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let mut cfg = Config::from_env();
    if let Some(p) = db_path {
        cfg.db_path = p;
    }
    let pool = db::connect(&cfg.db_path).await?;

    let mut inserted = 0u64;
    let mut updated = 0u64;
    let mut skipped = 0u64;
    let mut deleted = 0u64;

    if let Some(path) = from_9router_backup {
        let raw = std::fs::read_to_string(&path)?;
        let v: Value = serde_json::from_str(&raw)?;

        let accounts = import_util::parse_9router_backup(&v);
        let total_parsed = accounts.len();

        if replace {
            deleted = db::delete_accounts_by_providers(
                &pool,
                import_util::SUPPORTED_PROVIDERS,
            )
            .await?;
            eprintln!("replace: deleted {deleted} existing accounts");
        }

        for acc in accounts {
            let id = acc.id.clone();
            match db::upsert_account(&pool, &acc).await {
                Ok(db::UpsertKind::Inserted) => inserted += 1,
                Ok(db::UpsertKind::Updated) => updated += 1,
                Err(e) => {
                    eprintln!("skip {id}: {e}");
                    skipped += 1;
                }
            }
        }

        println!(
            "9router-backup import done: parsed={total_parsed} inserted={inserted} updated={updated} skipped={skipped} deleted={deleted} db={}",
            cfg.db_path.display()
        );
        return Ok(());
    }

    if replace {
        deleted = db::delete_accounts_by_providers(
            &pool,
            import_util::SUPPORTED_PROVIDERS,
        )
        .await?;
        eprintln!("replace: deleted {deleted} existing accounts");
    }

    if let Some(path) = file {
        let raw = std::fs::read_to_string(&path)?;
        let v: Value = serde_json::from_str(&raw)?;

        if import_util::is_9router_backup(&v) {
            let accounts = import_util::parse_9router_backup(&v);
            let total_parsed = accounts.len();
            for acc in accounts {
                let id = acc.id.clone();
                match db::upsert_account(&pool, &acc).await {
                    Ok(db::UpsertKind::Inserted) => inserted += 1,
                    Ok(db::UpsertKind::Updated) => updated += 1,
                    Err(e) => {
                        eprintln!("skip {id}: {e}");
                        skipped += 1;
                    }
                }
            }
            println!(
                "9router-backup import done: parsed={total_parsed} inserted={inserted} updated={updated} skipped={skipped} deleted={deleted} db={}",
                cfg.db_path.display()
            );
            return Ok(());
        }

        let items = extract_items(&v);
        for item in items {
            match import_one(&pool, &provider, &item).await {
                Ok(true) => inserted += 1,
                Ok(false) => updated += 1,
                Err(e) => {
                    eprintln!("skip: {e}");
                    skipped += 1;
                }
            }
        }
    } else if let Some(path) = from_9router {
        let opts = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .read_only(true);
        let src = sqlx::SqlitePool::connect_with(opts).await?;
        let rows = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<i64>, String)>(
            r#"SELECT id, email, name, isActive, data FROM providerConnections WHERE provider = ?"#,
        )
        .bind(&provider)
        .fetch_all(&src)
        .await;

        match rows {
            Ok(list) => {
                for (id, email, name, is_active, data) in list {
                    let now = db::now_rfc3339();
                    let (q_lim, q_rem) = db::default_quota_for_provider(&provider);
                    let acc = Account {
                        id,
                        provider: provider.clone(),
                        email,
                        name,
                        is_active: is_active.unwrap_or(1),
                        priority: 0,
                        data: normalize_json_str(&data),
                        cooldown_until: None,
                        last_error: None,
                        last_used_at: None,
                        created_at: now.clone(),
                        updated_at: now,
                        quota_limit: q_lim,
                        quota_remaining: q_rem,
                    };
                    match db::upsert_account(&pool, &acc).await? {
                        db::UpsertKind::Inserted => inserted += 1,
                        db::UpsertKind::Updated => updated += 1,
                    }
                }
            }
            Err(e) => {
                eprintln!("9Router query failed: {e}");
                eprintln!("Tip: use --from-9router-backup with JSON export instead.");
                skipped += 1;
            }
        }
    } else {
        usage();
        std::process::exit(1);
    }

    println!(
        "import done: inserted={inserted} updated={updated} skipped={skipped} deleted={deleted} db={}",
        cfg.db_path.display()
    );
    Ok(())
}

fn extract_items(v: &Value) -> Vec<Value> {
    if let Some(a) = v.as_array() {
        return a.clone();
    }
    if let Some(a) = v.get("accounts").and_then(|x| x.as_array()) {
        return a.clone();
    }
    if let Some(a) = v.get("results").and_then(|x| x.as_array()) {
        return a.clone();
    }
    vec![v.clone()]
}

async fn import_one(
    pool: &sqlx::SqlitePool,
    provider: &str,
    item: &Value,
) -> Result<bool, Box<dyn std::error::Error>> {
    let email = item
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let name = item
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let data_val = if let Some(d) = item.get("data") {
        normalize_map(d)
    } else {
        normalize_map(item)
    };

    let now = db::now_rfc3339();
    let id = item
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let (q_lim, q_rem) = db::default_quota_for_provider(provider);
    let acc = Account {
        id,
        provider: provider.to_string(),
        email,
        name,
        is_active: 1,
        priority: 0,
        data: data_val.to_string(),
        cooldown_until: None,
        last_error: None,
        last_used_at: None,
        created_at: now.clone(),
        updated_at: now,
        quota_limit: q_lim,
        quota_remaining: q_rem,
    };
    match db::upsert_account(pool, &acc).await? {
        db::UpsertKind::Inserted => Ok(true),
        db::UpsertKind::Updated => Ok(false),
    }
}

fn normalize_json_str(s: &str) -> String {
    match serde_json::from_str::<Value>(s) {
        Ok(v) => normalize_map(&v).to_string(),
        Err(_) => s.to_string(),
    }
}

fn normalize_map(v: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Value::Object(m) = v {
        for (k, val) in m {
            if matches!(
                k.as_str(),
                "provider" | "email" | "name" | "id" | "is_active" | "priority"
            ) {
                continue;
            }
            let nk = match k.as_str() {
                "access_token" => "accessToken",
                "refresh_token" => "refreshToken",
                "expires_at" => "expiresAt",
                "expires_in" => "expiresIn",
                "client_id" => "clientId",
                "id_token" => "idToken",
                "personal_token" => "personalToken",
                "api_key" => "apiKey",
                other => other,
            };
            out.insert(nk.to_string(), val.clone());
        }
    }
    Value::Object(out)
}
