use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct RtspRequest {
    pub method: String,
    pub url: String,
    pub version: String,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
}

impl RtspRequest {
    pub fn new(method: &str, url: &str) -> Self {
        Self {
            method: method.to_string(),
            url: url.to_string(),
            version: "RTSP/1.0".to_string(),
            headers: HashMap::new(),
            body: None,
        }
    }

    pub fn header(mut self, key: &str, value: &str) -> Self {
        self.headers.insert(key.to_string(), value.to_string());
        self
    }

    pub fn body(mut self, body: &str) -> Self {
        self.body = Some(body.to_string());
        self
    }

    pub fn to_string(&self) -> String {
        let mut result = format!("{} {} {}\r\n", self.method, self.url, self.version);
        
        for (key, value) in &self.headers {
            result.push_str(&format!("{}: {}\r\n", key, value));
        }
        
        if let Some(body) = &self.body {
            result.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        
        result.push_str("\r\n");
        
        if let Some(body) = &self.body {
            result.push_str(body);
        }
        
        result
    }

    pub fn parse(input: &str) -> Option<Self> {
        let mut lines = input.lines();
        let first_line = lines.next()?;
        
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        if parts.len() < 3 {
            return None;
        }
        
        let method = parts[0].to_string();
        let url = parts[1].to_string();
        let version = parts[2].to_string();
        
        let mut headers = HashMap::new();
        let mut content_length = 0;
        
        for line in lines {
            if line.is_empty() {
                break;
            }
            
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_string();
                let value = value.trim().to_string();
                
                if key.eq_ignore_ascii_case("Content-Length") {
                    content_length = value.parse::<usize>().unwrap_or(0);
                }
                
                headers.insert(key, value);
            }
        }
        
        let body_start = input.find("\r\n\r\n").map(|p| p + 4).unwrap_or(input.len());
        let body = if content_length > 0 && body_start + content_length <= input.len() {
            Some(input[body_start..body_start + content_length].to_string())
        } else if body_start < input.len() {
            Some(input[body_start..].to_string())
        } else {
            None
        };
        
        Some(Self {
            method,
            url,
            version,
            headers,
            body,
        })
    }

    pub fn get_header(&self, key: &str) -> Option<&str> {
        self.headers.get(key).map(|s| s.as_str())
    }

    pub fn cseq(&self) -> &str {
        self.get_header("CSeq").unwrap_or("0")
    }
}

#[derive(Debug, Clone)]
pub struct RtspResponse {
    pub status_code: u32,
    pub reason_phrase: String,
    pub version: String,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
}

impl RtspResponse {
    pub fn new(status_code: u32, reason_phrase: &str) -> Self {
        Self {
            status_code,
            reason_phrase: reason_phrase.to_string(),
            version: "RTSP/1.0".to_string(),
            headers: HashMap::new(),
            body: None,
        }
    }

    pub fn header(mut self, key: &str, value: &str) -> Self {
        self.headers.insert(key.to_string(), value.to_string());
        self
    }

    pub fn body(mut self, body: &str) -> Self {
        self.body = Some(body.to_string());
        self
    }

    pub fn with_cseq(mut self, cseq: &str) -> Self {
        self.headers.insert("CSeq".to_string(), cseq.to_string());
        self
    }

    pub fn to_string(&self) -> String {
        let mut result = format!("{} {} {}\r\n", self.version, self.status_code, self.reason_phrase);
        
        for (key, value) in &self.headers {
            // Skip Content-Length here as it's added automatically below
            if key.eq_ignore_ascii_case("Content-Length") {
                continue;
            }
            result.push_str(&format!("{}: {}\r\n", key, value));
        }
        
        if let Some(body) = &self.body {
            result.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        
        result.push_str("\r\n");
        
        if let Some(body) = &self.body {
            result.push_str(body);
        }
        
        result
    }

    pub fn parse(input: &str) -> Option<Self> {
        let mut lines = input.lines();
        let first_line = lines.next()?;
        
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        if parts.len() < 3 {
            return None;
        }
        
        let version = parts[0].to_string();
        let status_code = parts[1].parse::<u32>().ok()?;
        let reason_phrase = parts[2..].join(" ");
        
        let mut headers = HashMap::new();
        let mut content_length = 0;
        
        for line in lines {
            if line.is_empty() {
                break;
            }
            
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_string();
                let value = value.trim().to_string();
                
                if key.eq_ignore_ascii_case("Content-Length") {
                    content_length = value.parse::<usize>().unwrap_or(0);
                }
                
                headers.insert(key, value);
            }
        }
        
        let body_start = input.find("\r\n\r\n").map(|p| p + 4).unwrap_or(input.len());
        let body = if content_length > 0 && body_start + content_length <= input.len() {
            Some(input[body_start..body_start + content_length].to_string())
        } else if body_start < input.len() {
            Some(input[body_start..].to_string())
        } else {
            None
        };
        
        Some(Self {
            status_code,
            reason_phrase,
            version,
            headers,
            body,
        })
    }

    pub fn get_header(&self, key: &str) -> Option<&str> {
        self.headers.get(key).map(|s| s.as_str())
    }

    pub fn cseq(&self) -> &str {
        self.get_header("CSeq").unwrap_or("0")
    }
}