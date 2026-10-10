// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Pass 3 (typed deserialization) error reporting: the model path of a
//! serde failure, and a hint for a property the `SERVICE` extension renamed
//! or removed while RFC 0009 was drafted.
//!
//! `serde` reports an `unknown field`, `missing field` or `invalid type`
//! with no record of *where* in the document it was. The decode functions
//! deserialize through [`serde_path_to_error`], which tracks the path, and
//! this module turns the result into one [`ValidationErrors`] entry at that
//! path — `steps[0] -> stepServices`, `services[0] -> healthCheck` — so a
//! typed-deserialization failure reads like every other validation error.
//! The serde message is kept verbatim; a hint, when one applies, follows it
//! on the same line.
//!
//! The hints cover the property names that earlier drafts of RFC 0009 used
//! and that a template written against them still carries:
//!
//! | written | now |
//! |---|---|
//! | `jobServices` (root) | `services`, with `service: <name>` on each Step |
//! | `stepServices` (a Step) | the top-level `services`, with `service: <name>` on the Step |
//! | `serviceEnvironments` (root) | removed; `onEnter`, or a Job Environment with `runScope: [SERVICE]` |
//! | `requiresServices` (Environment Template root) | a Job Template property; an Environment Template provides Services in `services` |
//! | `readinessCheck` (a Service) | `healthCheck` |
//! | `onReadinessCheck` (`<ServiceActions>`) | `onHealthCheck` |
//! | `onWrapServiceReadinessCheck` (`<EnvironmentActions>`) | `onWrapServiceHealthCheck` |
//! | `timeoutSeconds`, `readyTimeoutSeconds` (a health check) | `readinessTimeoutSeconds` |
//! | `intervalSeconds` (a health check) | `readinessIntervalSeconds` before READY, `healthIntervalSeconds` after |
//! | `readinessIntervalSeconds` on a `STDOUT` check | does not apply: the ready line arrives when it arrives |
//! | `ports` missing from a Service | a Service declares at least one port |

use crate::error::{PathElement, ValidationErrors};

/// Convert a typed-deserialization failure at `path` (as
/// [`serde_path_to_error`] tracked it) into one validation error for
/// `model_name`, with any applicable hint appended.
pub(crate) fn deserialization_error(
    path: &serde_path_to_error::Path,
    message: &str,
) -> ValidationErrors {
    let path = model_path(path);
    let mut errors = ValidationErrors::default();
    let message = match hint(&path, message) {
        Some(hint) => format!("{message}. {hint}"),
        None => message.to_string(),
    };
    errors.add(&path, message);
    errors
}

/// The [`PathElement`] form of a serde path. A map key and a struct field
/// are both fields; a sequence index is an index; the `Unknown` segment a
/// deserializer emits when it cannot say is dropped. The segments of a
/// serde path are `Map { key }`, `Seq { index }`, `Enum { variant }` and
/// `Unknown`; an enum variant does not appear in a document path.
fn model_path(path: &serde_path_to_error::Path) -> Vec<PathElement> {
    use serde_path_to_error::Segment;
    path.iter()
        .filter_map(|seg| match seg {
            Segment::Map { key } => Some(PathElement::Field(key.clone())),
            Segment::Seq { index } => Some(PathElement::Index(*index)),
            Segment::Enum { .. } | Segment::Unknown => None,
        })
        .collect()
}

/// The field a `unknown field \`X\`` or `missing field \`X\`` message names.
fn named_field(message: &str) -> Option<(&'static str, &str)> {
    for kind in ["unknown field", "missing field"] {
        if let Some(rest) = message.strip_prefix(kind) {
            let rest = rest.strip_prefix(" `")?;
            let end = rest.find('`')?;
            return Some((kind, &rest[..end]));
        }
    }
    None
}

/// True when `path` ends inside a `<Service>` — `services[k]` of either
/// template root — at `depth` levels below the Service (0: the Service's
/// own fields; 1: a field of one of its properties).
fn within_service(path: &[PathElement], depth: usize, property: Option<&str>) -> bool {
    let n = path.len();
    if n < 2 + depth {
        return false;
    }
    let base = &path[..n - depth];
    let [.., PathElement::Field(list), PathElement::Index(_)] = base else {
        return false;
    };
    if list != "services" {
        return false;
    }
    match property {
        Some(property) => matches!(&path[n - depth], PathElement::Field(f) if f == property),
        None => true,
    }
}

/// The rename hint for the serde failure `message` at `path`, if any.
fn hint(path: &[PathElement], message: &str) -> Option<String> {
    let last_field = path.iter().rev().find_map(|e| match e {
        PathElement::Field(f) => Some(f.as_str()),
        PathElement::Index(_) => None,
    });
    let (kind, field) = named_field(message)?;
    if kind == "missing field" {
        // `missing field` is reported on the struct, so the path ends at
        // the Service itself.
        if field == "ports" && within_service(path, 0, None) {
            return Some(
                "A Service declares at least one port in 'ports'; a port-less background \
                 process is not a Service."
                    .to_string(),
            );
        }
        return None;
    }
    // An unknown field of a struct is reported at `…-> <field>`; of an
    // internally tagged enum (`healthCheck`) at the enum's own path.
    let in_health_check = within_service(path, 1, Some("healthCheck"))
        || (last_field == Some("healthCheck") && within_service(path, 0, None))
        || (last_field == Some(field) && within_service(path, 2, Some("healthCheck")));
    let at_root = path.len() == 1;
    let on_step = matches!(path, [PathElement::Field(f), PathElement::Index(_), PathElement::Field(g)] if f == "steps" && g == field);
    let on_service = within_service(path, 1, Some(field));
    let in_actions = last_field == Some(field)
        && path
            .iter()
            .any(|e| matches!(e, PathElement::Field(f) if f == "actions"));
    let text = match field {
        "jobServices" if at_root => {
            "'jobServices' is not a property; declare Services in 'services' and put each Step \
             in a Service's scope with 'service: <name>' in the Step's dependencies."
        }
        "stepServices" if on_step => {
            "'stepServices' is not a property; move the Service to the top-level 'services' list \
             and add 'service: <name>' to this Step's dependencies."
        }
        // Only the Environment Template root rejects it; a Job Template's
        // is a known field.
        "requiresServices" if at_root => {
            "'requiresServices' is a Job Template property; an Environment Template declares the \
             Services it provides in 'services'."
        }
        "serviceEnvironments" if at_root => {
            "'serviceEnvironments' is not a property; a Service sets up its own host in \
             'onEnter', or a Job Environment with 'runScope: [SERVICE]' is entered by every \
             Service Session."
        }
        "readinessCheck" if on_service => {
            "'readinessCheck' is not a property; the health check is 'healthCheck'."
        }
        "onReadinessCheck" if in_actions => {
            "'onReadinessCheck' is not a property; the health check action is 'onHealthCheck'."
        }
        "onWrapServiceReadinessCheck" if in_actions => {
            "'onWrapServiceReadinessCheck' is not a property; the hook is \
             'onWrapServiceHealthCheck'."
        }
        "timeoutSeconds" | "readyTimeoutSeconds" if in_health_check => {
            return Some(format!(
                "'{field}' is not a property of a health check; the time allowed to become READY \
                 is 'readinessTimeoutSeconds'."
            ));
        }
        "intervalSeconds" if in_health_check => {
            "'intervalSeconds' is not a property of a health check; use \
             'readinessIntervalSeconds' for probes before READY and 'healthIntervalSeconds' for \
             probes after."
        }
        "readinessIntervalSeconds" if in_health_check => {
            "'readinessIntervalSeconds' does not apply to a STDOUT health check: the ready line \
             arrives when it arrives."
        }
        _ => return None,
    };
    Some(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(fields: &[&str]) -> Vec<PathElement> {
        fields
            .iter()
            .map(|f| match f.parse::<usize>() {
                Ok(i) => PathElement::Index(i),
                Err(_) => PathElement::Field(f.to_string()),
            })
            .collect()
    }

    #[test]
    fn named_field_parses_both_kinds() {
        assert_eq!(
            named_field("unknown field `x`, expected one of `a`"),
            Some(("unknown field", "x"))
        );
        assert_eq!(
            named_field("missing field `ports`"),
            Some(("missing field", "ports"))
        );
        assert_eq!(named_field("invalid type: map, expected a string"), None);
    }

    #[test]
    fn hints_apply_only_at_their_sites() {
        let unknown = |f: &str| format!("unknown field `{f}`, expected one of `a`");
        assert!(hint(&p(&["jobServices"]), &unknown("jobServices")).is_some());
        assert!(hint(&p(&["steps", "0", "jobServices"]), &unknown("jobServices")).is_none());
        assert!(hint(
            &p(&["steps", "0", "stepServices"]),
            &unknown("stepServices")
        )
        .is_some());
        assert!(hint(
            &p(&["services", "0", "readinessCheck"]),
            &unknown("readinessCheck")
        )
        .is_some());
        assert!(hint(
            &p(&["steps", "0", "readinessCheck"]),
            &unknown("readinessCheck")
        )
        .is_none());
        // The tagged enum reports at the enum's path…
        assert!(hint(
            &p(&["services", "0", "healthCheck"]),
            &unknown("timeoutSeconds")
        )
        .is_some());
        assert!(hint(
            &p(&["services", "0", "healthCheck"]),
            &unknown("intervalSeconds")
        )
        .is_some());
        // …and a plain struct at the field's.
        assert!(hint(
            &p(&["services", "0", "healthCheck", "readyTimeoutSeconds"]),
            &unknown("readyTimeoutSeconds")
        )
        .is_some());
        assert!(hint(
            &p(&["steps", "0", "script", "actions", "onRun", "timeoutSeconds"]),
            &unknown("timeoutSeconds")
        )
        .is_none());
        assert!(hint(
            &p(&["services", "0", "script", "actions", "onReadinessCheck"]),
            &unknown("onReadinessCheck")
        )
        .is_some());
        assert!(hint(
            &p(&[
                "jobEnvironments",
                "0",
                "script",
                "actions",
                "onWrapServiceReadinessCheck"
            ]),
            &unknown("onWrapServiceReadinessCheck")
        )
        .is_some());
        assert!(hint(&p(&["services", "0"]), "missing field `ports`").is_some());
        assert!(hint(&p(&["requiresServices", "0"]), "missing field `ports`").is_none());
        assert!(hint(&p(&["services", "0"]), "missing field `script`").is_none());
    }
}
