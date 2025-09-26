use std::collections::HashMap;

pub type StringMap = HashMap<String, String>;

#[allow(unused)]
const BINCODE_CONFIG: bincode::config::Configuration = bincode::config::standard();

pub trait StringMapExt {
    fn to_bytes(&self) -> Vec<u8>;
    fn from_bytes(data: &[u8]) -> Self;
}

impl StringMapExt for StringMap {
    /// StringMap 序列化为字节数组
    /// 当启用"depth"feature时，使用bincode序列化；否则使用"key=value\n"格式
    fn to_bytes(&self) -> Vec<u8> {
        #[cfg(feature = "depth")]
        {
            bincode::encode_to_vec(self, BINCODE_CONFIG).expect("serialize")
        }
        // ## 其他实现方法的问题：
        // ```rust
        // let lines: Vec<String> = self
        //     .iter()
        //     .map(|(k, v)| format!("{}={}", k, v))  // 多次内存分配
        //     .collect();
        // lines.join("\n").into_bytes()  // 又一次内存分配
        // ```
        #[cfg(not(feature = "depth"))]
        {
            if self.is_empty() {
                return Vec::new();
            }

            // 预估总容量：每项 key + '=' + value + '\n'（最后一项我们去掉末尾 '\n'）
            let mut cap = 0usize;
            for (k, v) in self.iter() {
                cap += k.len() + 1 + v.len() + 1;
            }
            cap -= 1;

            let mut out = Vec::with_capacity(cap);
            let len = self.len();
            for (i, (k, v)) in self.iter().enumerate() {
                out.extend_from_slice(k.as_bytes());
                out.push(b'=');
                out.extend_from_slice(v.as_bytes());
                if i + 1 != len {
                    out.push(b'\n');
                }
            }
            out
        }
    }

    /// 从字节数组反序列化为 StringMap
    /// 当启用"depth"feature时，使用bincode反序列化；否则使用"key=value\n"格式
    fn from_bytes(data: &[u8]) -> Self {
        #[cfg(feature = "depth")]
        {
            bincode::decode_from_slice(data, BINCODE_CONFIG).expect("deserialize").0
        }
        // ## 其他实现方法的问题：
        // ```rust
        // let content = String::from_utf8_lossy(data);
        // let mut map = HashMap::new();
        // for line in content.lines() {
        //     if let Some((key, value)) = line.split_once('=') {
        //         map.insert(key.to_string(), value.to_string());  // 多次内存分配
        //     }
        // }
        // map
        // ```
        #[cfg(not(feature = "depth"))]
        {
            let mut map = HashMap::new();
            if data.is_empty() {
                return map;
            }

            // 估算行数并 reserve，减少 rehash。
            let approx_entries = 1 + data.iter().filter(|&&b| b == b'\n').count();
            map.reserve(approx_entries);

            // 优先尝试零拷贝的 UTF-8 验证
            if let Ok(s) = std::str::from_utf8(data) {
                for line in s.split('\n') {
                    if line.is_empty() { continue; }
                    if let Some(pos) = line.find('=') {
                        let key = &line[..pos];
                        let val = &line[pos + 1..];
                        map.insert(key.to_owned(), val.to_owned());
                    }
                }
            } else {
                // 遇到非 UTF-8，降级到 lossy（只在必须时使用）
                for line in String::from_utf8_lossy(data).lines() {
                    if let Some((k, v)) = line.split_once('=') {
                        map.insert(k.to_owned(), v.to_owned());
                    }
                }
            }
            map
        }
    }
}