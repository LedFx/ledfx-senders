//! OSC packet layout. Setup paths are already Python str.format-expanded UTF-8.
pub struct Layout {
    pub packets: Vec<Vec<u8>>,
    offsets: Vec<usize>,
    width: usize,
}
impl Layout {
    pub fn new(count: usize, mode: &str, paths: Vec<Vec<u8>>) -> Result<Self, &'static str> {
        let (width, tags) = match mode {
            "One_Argument" => (3, ",[fff]".to_owned()),
            "Three_Arguments" => (3, ",fff".to_owned()),
            "Three_Addresses" => (1, ",f".to_owned()),
            "All_To_One" => (count, format!(",{}", "[fff]".repeat(count / 3))),
            _ => return Err("invalid OSC send type"),
        };
        if paths.len() != count / width {
            return Err("incorrect OSC path count");
        }
        let mut packets = Vec::with_capacity(paths.len());
        let mut offsets = Vec::with_capacity(paths.len());
        for path in paths {
            if path.first() != Some(&b'/')
                || path.contains(&0)
                || std::str::from_utf8(&path).is_err()
            {
                return Err("invalid OSC address");
            }
            let mut p = path;
            p.resize((p.len() + 4) & !3, 0);
            p.extend_from_slice(tags.as_bytes());
            p.resize((p.len() + 4) & !3, 0);
            offsets.push(p.len());
            p.resize(p.len() + width * 4, 0);
            if p.len() > 65507 {
                return Err("OSC datagram exceeds UDP ceiling");
            }
            packets.push(p);
        }
        Ok(Self {
            packets,
            offsets,
            width,
        })
    }
    pub fn pack(&mut self, values: &[f32]) {
        for ((packet, &offset), values) in self
            .packets
            .iter_mut()
            .zip(&self.offsets)
            .zip(values.chunks_exact(self.width))
        {
            for (slot, value) in packet[offset..].chunks_exact_mut(4).zip(values) {
                slot.copy_from_slice(&value.to_be_bytes());
            }
        }
    }
}
