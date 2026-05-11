use std::str::FromStr;

use portunus_auth::{RbacError, UserId};

pub(crate) fn parse_stored_user_id(raw: &str) -> Result<UserId, RbacError> {
    if raw.starts_with('_') {
        Ok(UserId::reserved(raw))
    } else {
        UserId::from_str(raw)
    }
}
