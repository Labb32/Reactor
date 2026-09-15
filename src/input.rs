use std::{fs, io::Read, path::Path};

use serde::de::DeserializeOwned;

use crate::error::{ReactorError, Result, io_error};

pub fn read_document<T: DeserializeOwned>(source: &str) -> Result<T> {
    let bytes = if source == "-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .read_to_end(&mut bytes)
            .map_err(|error| io_error("stdin", error))?;
        bytes
    } else {
        fs::read(source).map_err(|error| io_error(source, error))?
    };

    parse_document(source, &bytes)
}

fn parse_document<T: DeserializeOwned>(source: &str, bytes: &[u8]) -> Result<T> {
    match Path::new(source)
        .extension()
        .and_then(|value| value.to_str())
    {
        Some(extension) if extension.eq_ignore_ascii_case("json") => serde_json::from_slice(bytes)
            .map_err(|error| ReactorError::InvalidInput(error.to_string())),
        Some(extension)
            if extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml") =>
        {
            serde_yaml_ng::from_slice(bytes)
                .map_err(|error| ReactorError::InvalidInput(error.to_string()))
        }
        _ => serde_json::from_slice(bytes).or_else(|json_error| {
            serde_yaml_ng::from_slice(bytes).map_err(|yaml_error| {
                ReactorError::InvalidInput(format!(
                    "input is neither valid JSON ({json_error}) nor YAML ({yaml_error})"
                ))
            })
        }),
    }
}
