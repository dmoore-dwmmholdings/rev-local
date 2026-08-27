/// Return the items on `page`, counting from zero.
pub fn page_items(items: &[String], page: usize, per_page: usize) -> Vec<String> {
    let start = page * per_page;
    let mut out = Vec::new();
    // BUG (planted): `<=` walks one past the last index on a full final page.
    for index in start..=(start + per_page) {
        if index <= items.len() {
            out.push(items[index].clone());
        }
    }
    out
}
