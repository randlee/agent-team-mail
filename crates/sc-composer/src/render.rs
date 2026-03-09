use std::collections::BTreeMap;
use std::path::Path;

use minijinja::{Environment, UndefinedBehavior};

use crate::ComposerError;

pub fn render_template(
    template_path: &Path,
    template_body: &str,
    context: &BTreeMap<String, String>,
) -> Result<String, ComposerError> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);

    let template =
        env.template_from_str(template_body)
            .map_err(|err| ComposerError::TemplateRender {
                path: template_path.to_path_buf(),
                message: err.to_string(),
            })?;

    template
        .render(context)
        .map_err(|err| ComposerError::TemplateRender {
            path: template_path.to_path_buf(),
            message: err.to_string(),
        })
}
