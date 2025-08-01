use std::{collections::HashMap, env::current_dir, path::Path};

use chrono::Utc;
use duct::cmd;
use heck::ToUpperCamelCase;
use rrgen::RRgen;
use serde_json::json;

use crate::{
    get_mappings, infer::parse_field_type, render_template, AppInfo, Error, GenerateResults, Result,
};

/// skipping some fields from the generated models.
/// For example, the `created_at` and `updated_at` fields are automatically
/// generated by the Loco app and should be given
pub const IGNORE_FIELDS: &[&str] = &["created_at", "updated_at", "create_at", "update_at"];

/// columns are <name>, <dbtype>: ("content", "string")
/// references are <to table, id col in from table>: ("user", `user_id`)
///  parsed from e.g.: model article content:string user:references
///  puts a `user_id` in articles, then fk to users
#[allow(clippy::type_complexity)]
pub fn get_columns_and_references(
    fields: &[(String, String)],
) -> Result<(Vec<(String, String)>, Vec<(String, String)>)> {
    let mut columns = Vec::new();
    let mut references = Vec::new();
    for (fname, ftype) in fields {
        if IGNORE_FIELDS.contains(&fname.as_str()) {
            tracing::warn!(
                field = fname,
                "note that a redundant field was specified, it is already generated automatically"
            );
            continue;
        }
        let field_type = parse_field_type(ftype)?;
        match field_type {
            crate::infer::FieldType::Reference => {
                // (users, "")
                references.push((fname.to_string(), String::new()));
            }
            crate::infer::FieldType::ReferenceWithCustomField(refname) => {
                references.push((fname.to_string(), refname.clone()));
            }
            crate::infer::FieldType::NullableReference => {
                references.push((format!("{fname}?"), String::new()));
            }
            crate::infer::FieldType::NullableReferenceWithCustomField(refname) => {
                references.push((format!("{fname}?"), refname.clone()));
            }
            crate::infer::FieldType::Type(ftype) => {
                let mappings = get_mappings();
                let col_type = mappings.col_type_field(ftype.as_str())?;
                columns.push((fname.to_string(), col_type.to_string()));
            }
            crate::infer::FieldType::TypeWithParameters(ftype, params) => {
                let mappings = get_mappings();
                let col_type = mappings.col_type_field(ftype.as_str())?;
                let arity = mappings.col_type_arity(ftype.as_str()).unwrap_or_default();
                if params.len() != arity {
                    return Err(Error::Message(format!(
                        "type: `{ftype}` requires specifying {arity} parameters, but only {} were \
                         given (`{}`).",
                        params.len(),
                        params.join(",")
                    )));
                }

                let col = match ftype.as_ref() {
                    "array" | "array^" | "array!" => {
                        let array_kind = match params.as_slice() {
                            [array_kind] => Ok(array_kind),
                            _ => Err(Error::Message(format!(
                                    "type: `{ftype}` requires exactly {arity} parameter{}, but {} were given (`{}`).",
                                    if arity == 1 { "" } else { "s" },
                                    params.len(),
                                    params.join(",")
                                ))),
                        }?;

                        format!(
                            r"{}(ArrayColType::{})",
                            col_type,
                            array_kind.to_upper_camel_case()
                        )
                    }
                    &_ => {
                        format!("{}({})", col_type, params.join(","))
                    }
                };

                columns.push((fname.to_string(), col));
            }
        }
    }
    Ok((columns, references))
}

pub fn generate(
    rrgen: &RRgen,
    name: &str,
    fields: &[(String, String)],
    appinfo: &AppInfo,
) -> Result<GenerateResults> {
    let pkg_name: &str = &appinfo.app_name;
    let ts = Utc::now();

    let (columns, references) = get_columns_and_references(fields)?;

    let vars = json!({"name": name, "ts": ts, "pkg_name": pkg_name, "columns": columns, "references": references});
    let gen_result = render_template(rrgen, Path::new("model"), &vars)?;

    if std::env::var("SKIP_MIGRATION").is_err() {
        // generate the model files by migrating and re-running seaorm
        let cwd = current_dir()?;
        let env_map: HashMap<_, _> = std::env::vars().collect();

        let _ = cmd!("cargo", "loco-tool", "db", "migrate",)
            .stderr_to_stdout()
            .dir(cwd.as_path())
            .full_env(&env_map)
            .run()
            .map_err(|err| {
                Error::Message(format!(
                    "failed to run loco db migration. error details: `{err}`",
                ))
            })?;
        let _ = cmd!("cargo", "loco-tool", "db", "entities",)
            .stderr_to_stdout()
            .dir(cwd.as_path())
            .full_env(&env_map)
            .run()
            .map_err(|err| {
                Error::Message(format!(
                    "failed to run loco db entities. error details: `{err}`",
                ))
            })?;
    }

    Ok(gen_result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_field(name: &str, field_type: &str) -> (String, String) {
        (name.to_string(), field_type.to_string())
    }

    #[test]
    fn test_get_columns_with_field_types() {
        let fields = [
            to_field("expect_string_null", "string"),
            to_field("expect_string", "string!"),
            to_field("expect_unique", "string^"),
        ];
        let res = get_columns_and_references(&fields).expect("Failed to parse fields");

        let expected_columns = vec![
            to_field("expect_string_null", "StringNull"),
            to_field("expect_string", "String"),
            to_field("expect_unique", "StringUniq"),
        ];
        let expected_references: Vec<(String, String)> = vec![];

        assert_eq!(res, (expected_columns, expected_references));
    }
    #[test]
    fn test_get_columns_with_array_types() {
        let fields = [
            to_field("expect_array_null", "array:string"),
            to_field("expect_array", "array!:string"),
            to_field("expect_array_uniq", "array^:string"),
        ];
        let res = get_columns_and_references(&fields).expect("Failed to parse fields");

        let expected_columns = vec![
            to_field("expect_array_null", "array_null(ArrayColType::String)"),
            to_field("expect_array", "array(ArrayColType::String)"),
            to_field("expect_array_uniq", "array_uniq(ArrayColType::String)"),
        ];
        let expected_references: Vec<(String, String)> = vec![];

        assert_eq!(res, (expected_columns, expected_references));
    }

    #[test]
    fn test_get_references_from_fields() {
        let fields = [
            to_field("user", "references"),
            to_field("post", "references"),
        ];
        let res = get_columns_and_references(&fields).expect("Failed to parse fields");

        let expected_columns: Vec<(String, String)> = vec![];
        let expected_references = vec![to_field("user", ""), to_field("post", "")];

        assert_eq!(res, (expected_columns, expected_references));
    }

    #[test]
    fn test_ignore_fields_are_filtered_out() {
        let mut fields = vec![to_field("name", "string")];

        for ignore_field in IGNORE_FIELDS {
            fields.push(to_field(ignore_field, "string"));
        }

        let res = get_columns_and_references(&fields).expect("Failed to parse fields");

        let expected_columns = vec![to_field("name", "StringNull")];
        let expected_references: Vec<(String, String)> = vec![];

        assert_eq!(res, (expected_columns, expected_references));
    }

    #[test]
    fn validate_arity() {
        // field not expected arity, but given 2
        let fields = vec![to_field("name", "string:2")];
        let res = get_columns_and_references(&fields);
        if let Err(err) = res {
            assert_eq!(
                err.to_string(),
                "type: `string` requires specifying 0 parameters, but only 1 were given (`2`)."
            );
        } else {
            panic!("Expected Err, but got Ok: {res:?}");
        }

        // references not expected arity, but given 2
        let references = vec![to_field("post:2", "")];
        let res = get_columns_and_references(&references);
        if let Err(err) = res {
            let mappings = get_mappings();
            assert_eq!(
                err.to_string(),
                mappings.error_unrecognized_default_field("").to_string()
            );
        } else {
            panic!("Expected Err, but got Ok: {res:?}");
        }
    }
}
