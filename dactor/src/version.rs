//! Wire protocol version for dactor cluster communication.
//!
//! The wire version determines whether two dactor nodes can form a cluster
//! and exchange messages. It is **independent** of the crate version in
//! `Cargo.toml` — a crate release may ship bug fixes or new features without
//! changing the wire format, in which case the wire version stays the same.
//!
//! # Compatibility Policy
//!
//! Two nodes are **compatible** if and only if they share the same MAJOR
//! wire version number. This is the *dactor wire-version policy*, not
//! generic semver:
//!
//! - Same MAJOR → compatible (e.g. `0.2.0` ↔ `0.3.0`)
//! - Different MAJOR → incompatible (e.g. `0.2.0` ↔ `1.0.0`)
//!
//! # When to Bump
//!
//! | Change | Bump |
//! |--------|------|
//! | Breaking wire format change | MAJOR |
//! | New optional wire feature (backward-compatible) | MINOR |
//! | Bug fix in serialization (no format change) | PATCH |
//! | New Rust API, new adapter, docs-only change | **None** — wire version unchanged |

use std::fmt;
use std::str::FromStr;

/// The current dactor wire protocol version.
///
/// This is a **frozen protocol constant**. It must only be bumped when the
/// wire format between nodes changes — never for Rust API changes, new
/// features, or documentation updates. See the module-level docs for the
/// bump policy.
pub const DACTOR_WIRE_VERSION: &str = "0.2.0";

// ---------------------------------------------------------------------------
// ParseWireVersionError
// ---------------------------------------------------------------------------

/// Error returned when parsing a wire version string fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseWireVersionError {
    input: String,
    reason: &'static str,
}

impl ParseWireVersionError {
    fn new(input: impl Into<String>, reason: &'static str) -> Self {
        Self {
            input: input.into(),
            reason,
        }
    }
}

impl fmt::Display for ParseWireVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid wire version \"{}\": {}",
            self.input, self.reason
        )
    }
}

impl std::error::Error for ParseWireVersionError {}

// ---------------------------------------------------------------------------
// WireVersion
// ---------------------------------------------------------------------------

/// Parsed wire protocol version with MAJOR.MINOR.PATCH components.
///
/// Use [`WireVersion::parse`] or `str::parse::<WireVersion>()` to create.
/// Use [`is_compatible`](WireVersion::is_compatible) to check whether two
/// nodes can cluster together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WireVersion {
    /// Breaking wire format changes.
    pub major: u32,
    /// New optional wire features (backward-compatible).
    pub minor: u32,
    /// Bug fixes in serialization (no format change).
    pub patch: u32,
}

impl WireVersion {
    /// Parse a `"MAJOR.MINOR.PATCH"` string into a [`WireVersion`].
    ///
    /// Returns an error if the string is not exactly three dot-separated
    /// non-negative integers (no whitespace, no extra segments).
    pub fn parse(s: &str) -> Result<Self, ParseWireVersionError> {
        s.parse()
    }

    /// Check whether `self` is compatible with `other` per the dactor
    /// wire-version policy: two versions are compatible if and only if
    /// they share the same MAJOR version number.
    pub fn is_compatible(&self, other: &WireVersion) -> bool {
        self.major == other.major
    }
}

impl FromStr for WireVersion {
    type Err = ParseWireVersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 3 {
            return Err(ParseWireVersionError::new(
                s,
                "expected exactly 3 dot-separated segments (MAJOR.MINOR.PATCH)",
            ));
        }

        let parse_segment = |seg: &str| -> Result<u32, ParseWireVersionError> {
            if seg.is_empty() {
                return Err(ParseWireVersionError::new(s, "empty version segment"));
            }
            // Reject leading zeros (except "0" itself) for canonical form
            if seg.len() > 1 && seg.starts_with('0') {
                return Err(ParseWireVersionError::new(
                    s,
                    "leading zeros are not allowed",
                ));
            }
            seg.parse::<u32>().map_err(|_| {
                ParseWireVersionError::new(s, "version segment is not a valid u32 integer")
            })
        };

        Ok(WireVersion {
            major: parse_segment(parts[0])?,
            minor: parse_segment(parts[1])?,
            patch: parse_segment(parts[2])?,
        })
    }
}

impl fmt::Display for WireVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl PartialOrd for WireVersion {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for WireVersion {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Constant regression ------------------------------------------------

    #[test]
    fn wire_version_constant_is_frozen() {
        // This test exists to catch accidental edits to the wire version.
        // If you need to bump it, update this assertion deliberately.
        assert_eq!(DACTOR_WIRE_VERSION, "0.2.0");
    }

    #[test]
    fn wire_version_constant_parses() {
        let v = WireVersion::parse(DACTOR_WIRE_VERSION).expect("constant must be parseable");
        assert_eq!(v.major, 0);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 0);
    }

    // -- Parsing valid versions ---------------------------------------------

    #[test]
    fn parse_simple() {
        let v: WireVersion = "1.2.3".parse().unwrap();
        assert_eq!(v, WireVersion { major: 1, minor: 2, patch: 3 });
    }

    #[test]
    fn parse_zero() {
        let v: WireVersion = "0.0.0".parse().unwrap();
        assert_eq!(v, WireVersion { major: 0, minor: 0, patch: 0 });
    }

    #[test]
    fn parse_large_numbers() {
        let v: WireVersion = "100.200.300".parse().unwrap();
        assert_eq!(v, WireVersion { major: 100, minor: 200, patch: 300 });
    }

    // -- Parsing invalid versions -------------------------------------------

    #[test]
    fn parse_rejects_two_segments() {
        assert!(WireVersion::parse("0.2").is_err());
    }

    #[test]
    fn parse_rejects_four_segments() {
        assert!(WireVersion::parse("0.2.0.1").is_err());
    }

    #[test]
    fn parse_rejects_non_numeric() {
        assert!(WireVersion::parse("a.b.c").is_err());
    }

    #[test]
    fn parse_rejects_whitespace() {
        assert!(WireVersion::parse(" 0.2.0 ").is_err());
        assert!(WireVersion::parse("0. 2.0").is_err());
    }

    #[test]
    fn parse_rejects_empty_segments() {
        assert!(WireVersion::parse("0..0").is_err());
        assert!(WireVersion::parse(".0.0").is_err());
        assert!(WireVersion::parse("0.0.").is_err());
    }

    #[test]
    fn parse_rejects_negative() {
        assert!(WireVersion::parse("-1.0.0").is_err());
    }

    #[test]
    fn parse_rejects_leading_zeros() {
        assert!(WireVersion::parse("01.2.3").is_err());
        assert!(WireVersion::parse("1.02.3").is_err());
        assert!(WireVersion::parse("1.2.03").is_err());
    }

    #[test]
    fn parse_rejects_empty_string() {
        assert!(WireVersion::parse("").is_err());
    }

    #[test]
    fn parse_rejects_overflow() {
        // u32::MAX + 1
        assert!(WireVersion::parse("4294967296.0.0").is_err());
    }

    // -- Compatibility (dactor wire-version policy) -------------------------

    #[test]
    fn compatible_same_version() {
        let v = WireVersion { major: 0, minor: 2, patch: 0 };
        assert!(v.is_compatible(&v));
    }

    #[test]
    fn compatible_same_major_different_minor() {
        let a = WireVersion::parse("0.2.0").unwrap();
        let b = WireVersion::parse("0.3.0").unwrap();
        assert!(a.is_compatible(&b));
        assert!(b.is_compatible(&a));
    }

    #[test]
    fn compatible_same_major_different_patch() {
        let a = WireVersion::parse("1.0.0").unwrap();
        let b = WireVersion::parse("1.0.5").unwrap();
        assert!(a.is_compatible(&b));
    }

    #[test]
    fn incompatible_different_major() {
        let a = WireVersion::parse("0.2.0").unwrap();
        let b = WireVersion::parse("1.0.0").unwrap();
        assert!(!a.is_compatible(&b));
        assert!(!b.is_compatible(&a));
    }

    // -- Ordering -----------------------------------------------------------

    #[test]
    fn ordering_major() {
        let a = WireVersion::parse("0.9.9").unwrap();
        let b = WireVersion::parse("1.0.0").unwrap();
        assert!(a < b);
    }

    #[test]
    fn ordering_minor() {
        let a = WireVersion::parse("0.2.0").unwrap();
        let b = WireVersion::parse("0.3.0").unwrap();
        assert!(a < b);
    }

    #[test]
    fn ordering_patch() {
        let a = WireVersion::parse("0.2.0").unwrap();
        let b = WireVersion::parse("0.2.1").unwrap();
        assert!(a < b);
    }

    #[test]
    fn ordering_chain() {
        let versions: Vec<WireVersion> = vec![
            "0.2.0", "0.2.1", "0.3.0", "1.0.0",
        ]
        .into_iter()
        .map(|s| s.parse().unwrap())
        .collect();

        for i in 0..versions.len() - 1 {
            assert!(versions[i] < versions[i + 1]);
        }
    }

    // -- Display ------------------------------------------------------------

    #[test]
    fn display_roundtrip() {
        let v = WireVersion::parse("1.2.3").unwrap();
        assert_eq!(v.to_string(), "1.2.3");
    }

    #[test]
    fn display_constant() {
        let v = WireVersion::parse(DACTOR_WIRE_VERSION).unwrap();
        assert_eq!(v.to_string(), DACTOR_WIRE_VERSION);
    }

    // -- Error display ------------------------------------------------------

    #[test]
    fn error_display() {
        let err = WireVersion::parse("bad").unwrap_err();
        assert!(err.to_string().contains("bad"));
        assert!(err.to_string().contains("invalid wire version"));
    }

    #[test]
    fn error_is_std_error() {
        let err = WireVersion::parse("x.y.z").unwrap_err();
        let _: &dyn std::error::Error = &err;
    }
}
