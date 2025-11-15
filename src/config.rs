use ::config::Config;

pub(crate) fn get_namespaced_value<T, F>(
    config: &Config,
    name: &str,
    key: &str,
    getter: F,
) -> Result<T, config::ConfigError>
where
    F: Fn(&Config, &str) -> Result<T, config::ConfigError>,
{
    if name.is_empty() {
        getter(config, key)
    } else {
        getter(config, &format!("{name}.{key}")).or_else(|_| getter(config, key))
    }
}

pub(crate) fn get_namespaced_usize(
    config: &Config,
    name: &str,
    key: &str,
) -> Result<usize, config::ConfigError> {
    get_namespaced_value(config, name, key, |cfg, key| cfg.get::<usize>(key))
}

pub(crate) fn get_namespaced_string(
    config: &Config,
    name: &str,
    key: &str,
) -> Result<String, config::ConfigError> {
    get_namespaced_value(config, name, key, Config::get_string)
}
