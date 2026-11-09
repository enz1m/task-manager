#[path = "first.rs"]
mod first;
#[path = "second.rs"]
mod second;

pub(super) fn key(value: &str) -> Option<&'static str> {
    [first::LABELS, second::LABELS]
        .into_iter()
        .find_map(|labels| {
            labels
                .binary_search_by_key(&value, |(source, _)| *source)
                .ok()
                .map(|index| labels[index].1)
        })
}
