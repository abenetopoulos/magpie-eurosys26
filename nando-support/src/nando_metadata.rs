use std::fmt::Display;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NandoKind {
    ReadOnly,
    ReadWrite,
    WriteOnly,
}

impl NandoKind {
    pub fn is_read_only(&self) -> bool {
        match self {
            NandoKind::ReadOnly => true,
            _ => false,
        }
    }

    pub fn is_write_only(&self) -> bool {
        match self {
            NandoKind::WriteOnly => true,
            _ => false,
        }
    }
}

impl Display for NandoKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadOnly => f.write_str("ReadOnly"),
            Self::ReadWrite => f.write_str("ReadWrite"),
            Self::WriteOnly => f.write_str("WriteOnly"),
        }
    }
}

#[derive(Copy, Clone)]
pub struct NandoMetadata {
    pub kind: NandoKind,
    pub spawns_nandos: bool,
    // NOTE if the nanotransaction kind is `WriteOnly` but this is `None`, then we are dealing with
    // some form of varargs.
    pub mutable_argument_indices: Option<&'static [usize]>,
    pub invalidate_on_completion: Option<&'static [usize]>,
}
