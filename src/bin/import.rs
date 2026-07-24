use marionette::config::Config;
use marionette::db::{self, Account};
use serde_json::Value;
use std::env;
use std::path::PathBuf;
use uuid::Uuid;

fn usage() {
    eprintln!(
        "Usage:
  marionette-import --file <path.json> [--provider grok-cli|qoder] [--db path]
  marionette-import --from-9router <data.sqlite> [--db path]

Env: MARIONETTE_DB (default ./data/marionette.sqlite)"
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
    let mut provider = "grok-cli".to_string();
    let mut db_path: Option<PathBuf> = None;

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
            "--provider" => {
                i += 1;
                provider = args[i].clone();
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

    if let Some(path) = file {
        let raw = std::fs::read_to_string(&path)?;
        let v: Value = serde_json::from_str(&raw)?;
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
        let url = format!("sqlite:{}?mode=ro", path.display());
        let src = sqlx::SqlitePool::connect(&url).await?;
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
                    };
                    let exists = db::get_account(&pool, &acc.id).await.is_ok();
                    db::upsert_account(&pool, &acc).await?;
                    if exists {
                        updated += 1;
                    } else {
                        inserted += 1;
                    }
                }
            }
            Err(e) => {
                eprintln!("9Router query failed: {e}");
                eprintln!("Tip: use --file with JSON export instead.");
                skipped += 1;
            }
        }
    } else {
        usage();
        std::process::exit(1);
    }

    println!(
        "import done: inserted={inserted} updated={updated} skipped={skipped} db={}",
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

    let existing = if let Some(ref e) = email {
        let rows = db::list_accounts(pool, Some(provider), None).await?;
        rows.into_iter()
            .find(|a| a.email.as_deref() == Some(e.as_str()))
    } else {
        None
    };

    let now = db::now_rfc3339();
    if let Some(mut acc) = existing {
        acc.data = data_val.to_string();
        if name.is_some() {
            acc.name = name;
        }
        acc.updated_at = now;
        db::update_account(pool, &acc).await?;
        Ok(false)
    } else {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
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
        };
        db::upsert_account(pool, &acc).await?;
        Ok(true)
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
                other => other,
            };
            out.insert(nk.to_string(), val.clone());
        }
    }
    Value::Object(out)
}
