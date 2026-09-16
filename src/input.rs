use std::{fs, io::Read, path::Path};

use serde::de::DeserializeOwned;

use crate::error::{ReactorError, Result, io_error};

const MAX_INPUT_BYTES: u64 = 4 * 1024 * 1024;

pub fn read_document<T: DeserializeOwned>(source: &str) -> Result<T> {
    let bytes = read_bytes(source)?;
    parse_document(source, &bytes)
}

pub fn read_text(source: &str) -> Result<String> {
    String::from_utf8(read_bytes(source)?)
        .map_err(|error| ReactorError::InvalidInput(error.to_string()))
}

fn read_bytes(source: &str) -> Result<Vec<u8>> {
    if source == "-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(MAX_INPUT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| io_error("stdin", error))?;
        validate_size(bytes)
    } else {
        read_file(Path::new(source))
    }
}

pub(crate) fn read_file(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|error| io_error(path, error))?
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(path, error))?;
    validate_size(bytes)
}

fn validate_size(bytes: Vec<u8>) -> Result<Vec<u8>> {
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(ReactorError::InvalidInput(
            "input document exceeds 4 MiB".to_owned(),
        ));
    }
    Ok(bytes)
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
