// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{LenkaError, Result};

/// The number of characters in the short form of an ID.
const SHORT_LEN: usize = 8;

/// Refuses a short ID that is too short to select one record with safety.
///
/// # Errors
///
/// Returns [`crate::ErrorCode::InvalidArgument`] if the text has fewer than eight characters.
pub fn check_short_id(text: &str) -> crate::Result<()> {
    if text.len() < SHORT_LEN {
        return Err(crate::LenkaError::invalid_argument(
            "An ID must be a full ID or a short ID from a tool result.",
        ));
    }
    Ok(())
}

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident, $label:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(Uuid);

        impl $name {
            /// Creates a new time-ordered ID.
            #[must_use]
            pub fn generate() -> Self {
                Self(Uuid::now_v7())
            }

            /// Parses the full form of an ID. Only the lowercase hyphenated form
            /// passes, so an ID is always safe in a file path.
            ///
            /// # Errors
            ///
            /// Returns [`crate::ErrorCode::InvalidArgument`] for any other form.
            pub fn parse(text: &str) -> Result<Self> {
                if !is_strict_uuid(text) {
                    return Err(LenkaError::invalid_argument(format!(
                        "{} ID is not valid. Use the full UUID form or a short ID from a tool result.",
                        $label
                    )));
                }
                Uuid::try_parse(text).map(Self).map_err(|_| {
                    LenkaError::invalid_argument(format!("{} ID is not valid.", $label))
                })
            }

            /// Returns the last eight characters of the ID.
            #[must_use]
            pub fn short(&self) -> String {
                let text = self.0.hyphenated().to_string();
                text[text.len() - SHORT_LEN..].to_owned()
            }

            /// Tells if the text is the short form of this ID. A shorter text never matches,
            /// because it can select the wrong record.
            #[must_use]
            pub fn matches_short(&self, short: &str) -> bool {
                short.len() >= SHORT_LEN && self.0.hyphenated().to_string().ends_with(short)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0.hyphenated())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0.hyphenated())
            }
        }

        impl FromStr for $name {
            type Err = LenkaError;

            fn from_str(text: &str) -> Result<Self> {
                Self::parse(text)
            }
        }

        impl TryFrom<String> for $name {
            type Error = LenkaError;

            fn try_from(text: String) -> Result<Self> {
                Self::parse(&text)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.to_string()
            }
        }
    };
}

fn is_strict_uuid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
}

id_type!(
    /// The stable ID of a project.
    ProjectId,
    "Project"
);
id_type!(
    /// The stable ID of a task.
    TaskId,
    "Task"
);
id_type!(
    /// The stable ID of an artifact.
    ArtifactId,
    "Artifact"
);
id_type!(
    /// The stable ID of a task entry.
    EntryId,
    "Entry"
);
id_type!(
    /// The stable ID of a shared memory record.
    MemoryId,
    "Memory"
);
id_type!(
    /// The stable ID of a change notice.
    NoticeId,
    "Notice"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_only_the_strict_form() {
        let id = TaskId::generate();
        assert_eq!(TaskId::parse(&id.to_string()).ok(), Some(id));
        for bad in [
            "",
            "../../etc/passwd",
            "019A5F3E-7C1D-7A42-9B1E-2F6D8C4A1E07",
            "019a5f3e7c1d7a429b1e2f6d8c4a1e07",
            "019a5f3e-7c1d-7a42-9b1e-2f6d8c4a1e0/",
        ] {
            assert!(TaskId::parse(bad).is_err(), "{bad} must fail");
        }
    }

    #[test]
    fn short_form_is_the_suffix() {
        let id = TaskId::generate();
        assert_eq!(id.short().len(), SHORT_LEN);
        assert!(id.matches_short(&id.short()));
        assert!(!id.matches_short(""));
        assert!(!id.matches_short(&id.short()[1..]));
        assert!(check_short_id("").is_err());
        assert!(check_short_id(&id.short()).is_ok());
    }

    #[test]
    fn new_ids_are_time_ordered() {
        let first = TaskId::generate();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(first < TaskId::generate());
    }
}
