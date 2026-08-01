//! Petites briques sans dependance : identifiants facon ES, horloge.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Un identifiant de 22 caracteres base64url, comme les `uuid` d'ES.
///
/// Pas de dependance a un generateur cryptographique : ces identifiants sont
/// cosmetiques (ils apparaissent dans `_cat/indices`, `/`), pas des secrets.
pub fn random_uuid() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut state = nanos ^ (n.wrapping_mul(0x9E37_79B9_7F4A_7C15)) ^ (std::process::id() as u64);
    let mut out = String::with_capacity(22);
    for _ in 0..22 {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let x = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        out.push(B64[(x >> 33) as usize % 64] as char);
    }
    out
}

/// Millisecondes depuis l'epoch.
pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
