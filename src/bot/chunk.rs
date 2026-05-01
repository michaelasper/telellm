pub fn chunk_for_telegram(input: &str, limit: usize) -> Vec<String> {
    assert!(limit > 0, "telegram chunk limit must be greater than zero");
    if input.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for ch in input.chars() {
        if current.len() + ch.len_utf8() > limit && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_for_telegram_should_return_empty_for_empty_input() {
        let chunks = chunk_for_telegram("", 10);

        assert!(chunks.is_empty());
    }

    #[test]
    fn chunk_for_telegram_should_split_by_byte_limit_without_breaking_utf8() {
        let chunks = chunk_for_telegram("ab😀cd", 4);

        assert_eq!(
            chunks,
            vec!["ab".to_owned(), "😀".to_owned(), "cd".to_owned()]
        );
    }

    #[test]
    fn chunk_for_telegram_should_keep_each_chunk_under_limit() {
        let chunks = chunk_for_telegram(&"x".repeat(9000), 3900);

        assert!(chunks.iter().all(|chunk| chunk.len() <= 3900));
    }
}
