//! Local setup commands: no server, environment-file loading or API-key logging.
use std::io::{self, Read};

use rand::{rngs::OsRng, RngCore};

use crate::{
    db,
    error::{AppError, AppResult},
};

pub fn hash_password(password: &str) -> AppResult<String> {
    if password.is_empty() || password.len() > 72 {
        return Err(AppError::BadRequest(
            "password must contain 1 to 72 UTF-8 bytes (bcrypt limit)".into(),
        ));
    }
    // Bounds above prevent truncation while retaining bcrypt's standard 72-byte
    // interoperability. The crate's non_truncating helper reserves a NUL byte.
    bcrypt::hash(password, bcrypt::DEFAULT_COST)
        .map_err(|_| AppError::BadRequest("password hashing failed".into()))
}

pub fn generate_totp_secret() -> AppResult<String> {
    let mut secret = [0u8; 20];
    OsRng
        .try_fill_bytes(&mut secret)
        .map_err(|_| AppError::BadRequest("secure randomness unavailable".into()))?;
    Ok(totp_rs::Secret::Raw(secret.to_vec())
        .to_encoded()
        .to_string())
}

pub fn execute(args: &[String]) -> AppResult<()> {
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["init-db", path] => {
            db::init_schema(&db::build_pool(path)?)?;
            println!("database initialized");
        }
        ["hash-password"] => {
            let password = rpassword::prompt_password("Password (at most 72 UTF-8 bytes): ")
                .map_err(|_| AppError::BadRequest("cannot read password from terminal".into()))?;
            let confirmation = rpassword::prompt_password("Confirm password: ")
                .map_err(|_| AppError::BadRequest("cannot read password from terminal".into()))?;
            if password != confirmation {
                return Err(AppError::BadRequest("passwords do not match".into()));
            }
            println!("{}", hash_password(&password)?);
        }
        ["hash-password", "--stdin"] => {
            let mut password = String::new();
            io::stdin().take(75).read_to_string(&mut password)
                .map_err(|_| AppError::BadRequest("cannot read password from stdin".into()))?;
            if password.ends_with('\n') {
                password.pop();
                if password.ends_with('\r') { password.pop(); }
            }
            println!("{}", hash_password(&password)?);
        }
        ["totp-secret"] => println!("{}", generate_totp_secret()?),
        ["--help"] | ["-h"] => println!("memento                         Start using .env.production (or MEMENTO_ENV=development)\nmemento init-db <path>          Initialize/upgrade a memento database\nmemento hash-password          Hash a password without terminal echo\nmemento hash-password --stdin  Hash a password read from stdin\nmemento totp-secret            Generate a Base32 TOTP enrollment secret"),
        _ => return Err(AppError::BadRequest("unknown arguments; use --help".into())),
    }
    Ok(())
}
