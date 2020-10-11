pub fn hexify(data: &[u8], maxlen: usize) -> String {
    data.iter()
        .take(maxlen)
        .map(|byte| format!("{:02x?}", byte))
        .collect::<Vec<String>>()
        .join("")
}

pub fn hexify_vec(data: &[&[u8]], maxlen: usize) -> Vec<String> {
    data.iter().map(|v| hexify(v, maxlen)).collect()
}

pub fn stringify(data: &[u8], maxlen: usize) -> String {
    String::from_utf8_lossy(data).chars().take(maxlen).collect()
}
