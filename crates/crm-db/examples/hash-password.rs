// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Operational helper: hash a staff password with Argon2id (task 03-SEC-03).
//!
//! Reads the password from stdin and prints the PHC-format hash string
//! (`$argon2id$v=19$...`) to stdout, ready for `staff_users.phc_hash`.
//! The password itself never appears in the output. See
//! `docs/operations/security.md` for the provisioning procedure.
//!
//! Run:
//!
//! ```sh
//! printf '%s' 'the-new-password' | cargo run -q -p crm-db --example hash-password
//! ```
//!
//! Prefer piping over a command-line argument so the password never lands
//! in shell history. A single trailing newline is stripped; anything else
//! is hashed verbatim.

use std::io::Read;

/// Reads the whole of stdin as the password, stripping one trailing
/// newline (from `echo` or interactive entry) so it is not hashed.
fn read_password() -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    std::io::stdin()
        .read_to_end(&mut raw)
        .map_err(|error| format!("failed to read password from stdin: {error}"))?;
    if raw.last() == Some(&b'\n') {
        raw.pop();
        if raw.last() == Some(&b'\r') {
            raw.pop();
        }
    }
    if raw.is_empty() {
        return Err("refusing to hash an empty password".to_owned());
    }
    Ok(raw)
}

#[tokio::main]
async fn main() {
    let password = match read_password() {
        Ok(password) => password,
        Err(error) => {
            eprintln!("hash-password: {error}");
            std::process::exit(2);
        }
    };
    match crm_db::auth::hash_password(&password).await {
        Ok(phc) => println!("{phc}"),
        Err(error) => {
            eprintln!("hash-password: password hashing failed: {error}");
            std::process::exit(1);
        }
    }
}
