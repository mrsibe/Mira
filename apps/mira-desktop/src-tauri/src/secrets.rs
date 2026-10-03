const SERVICE: &str = "mira";

fn account_for_model_config(id: &str) -> String {
    format!("model_config:{id}:api_key")
}

pub fn save_model_api_key(id: &str, api_key: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, &account_for_model_config(id))
        .map_err(|error| format!("Cannot open system credential store: {error}"))?;
    entry
        .set_password(api_key)
        .map_err(|error| format!("Cannot save API key to system credential store: {error}"))
}

pub fn load_model_api_key(id: &str) -> Result<Option<String>, String> {
    let entry = keyring::Entry::new(SERVICE, &account_for_model_config(id))
        .map_err(|error| format!("Cannot open system credential store: {error}"))?;
    match entry.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!(
            "Cannot read API key from system credential store: {error}"
        )),
    }
}

pub fn delete_model_api_key(id: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, &account_for_model_config(id))
        .map_err(|error| format!("Cannot open system credential store: {error}"))?;
    match entry.delete_password() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!(
            "Cannot delete API key from system credential store: {error}"
        )),
    }
}
