use std::fmt::Display;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

type BaseTxnIdType = u128;

#[derive(
    Copy, Clone, Default, Hash, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize,
)]
pub struct ActivationId {
    txn_id: BaseTxnIdType,
    subtxn_id: Ulid,
}

impl ActivationId {
    pub fn new_for_txn(txn_id: BaseTxnIdType) -> Self {
        Self {
            txn_id,
            subtxn_id: Ulid::nil(),
        }
    }

    pub fn new_subtxn(parent_activation_id: &Self) -> Self {
        Self {
            txn_id: parent_activation_id.txn_id,
            subtxn_id: Ulid::new(),
        }
    }

    pub fn belongs_to_root(&self) -> bool {
        self.subtxn_id.is_nil()
    }

    pub fn txn_id(&self) -> BaseTxnIdType {
        self.txn_id
    }
}

impl Display for ActivationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.subtxn_id.is_nil() {
            true => f.write_fmt(format_args!("{}.0", self.txn_id)),
            false => f.write_fmt(format_args!(
                "{}.{}",
                self.txn_id,
                self.subtxn_id.to_string()
            )),
        }
    }
}

impl FromStr for ActivationId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once('.') {
            Some((txn_id_str, subtxn_id_str)) => Ok(Self {
                txn_id: txn_id_str
                    .parse()
                    .expect("failed to convert txn id from str"),
                subtxn_id: match subtxn_id_str.len() == 1 {
                    true => Ulid::nil(),
                    false => Ulid::from_str(subtxn_id_str).expect("failed to parse subtxn id"),
                },
            }),
            None => Err(format!("invalid string for activation id {}", s)),
        }
    }
}
