use std::fmt;

#[derive(Debug, Clone)]
pub struct OwnershipSetError;

impl fmt::Display for OwnershipSetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Failed to set ownership of object collection")
    }
}
