pub fn compose_blocks(
    profile_body: &str,
    guidance_block: Option<&str>,
    user_prompt: Option<&str>,
) -> String {
    let mut out = String::new();
    for block in [
        profile_body.trim_end_matches('\n'),
        guidance_block.unwrap_or("").trim_end_matches('\n'),
        user_prompt.unwrap_or("").trim_end_matches('\n'),
    ] {
        if block.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(block);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::compose_blocks;

    #[test]
    fn compose_blocks_keeps_order_and_skips_empty() {
        let rendered = compose_blocks("profile", Some("guidance"), Some("prompt"));
        assert_eq!(rendered, "profile\n\nguidance\n\nprompt");

        let rendered = compose_blocks("profile", None, Some("prompt"));
        assert_eq!(rendered, "profile\n\nprompt");
    }
}
