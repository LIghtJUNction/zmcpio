//! Wire descriptions and result encoding; no registry, permissions, or execution.
use crate::{
    Error, ErrorKind,
    json::{Kind, Value},
    mcp::{Context, Era, Server},
    writer::Writer,
};

#[derive(Clone, Copy, Debug)]
pub struct Tool<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: Value<'a>,
    output_schema: Option<Value<'a>>,
}
impl<'a> Tool<'a> {
    /// Input schemas must be validated JSON Schema objects with type `object`.
    /// This checks the wire shape, not full JSON Schema semantics.
    pub fn new(name: &'a str, description: &'a str, schema: Value<'a>) -> Result<Self, Error> {
        if name.is_empty()
            || name.len() > 128
            || schema.kind() != Kind::Object
            || !schema
                .get("type")
                .and_then(Value::string)
                .is_some_and(|s| s.matches("object"))
        {
            return Err(Error::new(ErrorKind::InvalidParams, schema.offset()));
        }
        Ok(Self {
            name,
            description,
            input_schema: schema,
            output_schema: None,
        })
    }
    pub fn with_output_schema(mut self, schema: Value<'a>) -> Result<Self, Error> {
        if !matches!(schema.kind(), Kind::Object | Kind::Bool) {
            return Err(Error::new(ErrorKind::InvalidParams, schema.offset()));
        }
        self.output_schema = Some(schema);
        Ok(self)
    }
    pub fn name(self) -> &'a str {
        self.name
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Content<'a> {
    Text(&'a str),
    Image {
        data: &'a str,
        mime_type: &'a str,
    },
    Audio {
        data: &'a str,
        mime_type: &'a str,
    },
    ResourceLink {
        uri: &'a str,
        name: &'a str,
    },
    ResourceText {
        uri: &'a str,
        mime_type: &'a str,
        text: &'a str,
    },
    ResourceBlob {
        uri: &'a str,
        mime_type: &'a str,
        blob: &'a str,
    },
}
#[derive(Clone, Copy, Debug)]
pub struct ToolResult<'a> {
    pub content: &'a [Content<'a>],
    pub structured_content: Option<Value<'a>>,
    pub is_error: bool,
}
impl<'a> ToolResult<'a> {
    pub const fn new(content: &'a [Content<'a>]) -> Self {
        Self {
            content,
            structured_content: None,
            is_error: false,
        }
    }
}

impl Server<'_> {
    /// Caller supplies a deterministic order and pagination policy. No sorting allocation.
    pub fn list_tools(
        &self,
        context: Context<'_>,
        tools: &[Tool<'_>],
        next_cursor: Option<&str>,
        output: &mut [u8],
    ) -> Result<usize, Error> {
        for (i, tool) in tools.iter().enumerate() {
            if tools[..i].iter().any(|prior| prior.name == tool.name) {
                return Err(Error::new(ErrorKind::DuplicateKey, i));
            }
            if context.era == Era::Legacy
                && let Some(schema) = tool.output_schema
                && (schema.kind() != Kind::Object
                    || !schema
                        .get("type")
                        .and_then(Value::string)
                        .is_some_and(|s| s.matches("object")))
            {
                return Err(Error::new(ErrorKind::InvalidParams, schema.offset()));
            }
        }
        let mut w = Writer::new(output);
        self.begin_result(&mut w, context.request.id, context.era);
        w.raw(b"\"tools\":[");
        for (i, tool) in tools.iter().enumerate() {
            if i != 0 {
                w.raw(b",");
            }
            w.raw(b"{\"name\":");
            w.text(tool.name);
            w.raw(b",\"description\":");
            w.text(tool.description);
            w.raw(b",\"inputSchema\":");
            w.raw(tool.input_schema.raw().as_bytes());
            if let Some(schema) = tool.output_schema {
                w.raw(b",\"outputSchema\":");
                w.raw(schema.raw().as_bytes());
            }
            w.raw(b"}");
        }
        w.raw(b"]");
        if let Some(cursor) = next_cursor {
            w.raw(b",\"nextCursor\":");
            w.text(cursor);
        }
        w.raw(b"}}");
        w.finish()
    }
    /// Execution errors use `is_error`; protocol errors use `Server::error`.
    pub fn tool_result(
        &self,
        context: Context<'_>,
        result: ToolResult<'_>,
        output: &mut [u8],
    ) -> Result<usize, Error> {
        if context.era == Era::Legacy
            && result
                .structured_content
                .is_some_and(|v| v.kind() != Kind::Object)
        {
            return Err(Error::new(ErrorKind::InvalidParams, 0));
        }
        for block in result.content {
            let data = match block {
                Content::Image { data, .. } | Content::Audio { data, .. } => Some(*data),
                Content::ResourceBlob { blob, .. } => Some(*blob),
                _ => None,
            };
            if data.is_some_and(|s| !base64(s)) {
                return Err(Error::new(ErrorKind::InvalidParams, 0));
            }
        }
        let mut w = Writer::new(output);
        self.begin_result(&mut w, context.request.id, context.era);
        w.raw(b"\"content\":[");
        for (i, block) in result.content.iter().enumerate() {
            if i != 0 {
                w.raw(b",");
            }
            match block {
                Content::Text(text) => {
                    w.raw(b"{\"type\":\"text\",\"text\":");
                    w.text(text);
                    w.raw(b"}");
                }
                Content::Image { data, mime_type } | Content::Audio { data, mime_type } => {
                    w.raw(b"{\"type\":");
                    w.text(if matches!(block, Content::Image { .. }) {
                        "image"
                    } else {
                        "audio"
                    });
                    w.raw(b",\"data\":");
                    w.text(data);
                    w.raw(b",\"mimeType\":");
                    w.text(mime_type);
                    w.raw(b"}");
                }
                Content::ResourceLink { uri, name } => {
                    w.raw(b"{\"type\":\"resource_link\",\"uri\":");
                    w.text(uri);
                    w.raw(b",\"name\":");
                    w.text(name);
                    w.raw(b"}");
                }
                Content::ResourceText {
                    uri,
                    mime_type,
                    text,
                } => {
                    w.raw(b"{\"type\":\"resource\",\"resource\":{\"uri\":");
                    w.text(uri);
                    w.raw(b",\"mimeType\":");
                    w.text(mime_type);
                    w.raw(b",\"text\":");
                    w.text(text);
                    w.raw(b"}}");
                }
                Content::ResourceBlob {
                    uri,
                    mime_type,
                    blob,
                } => {
                    w.raw(b"{\"type\":\"resource\",\"resource\":{\"uri\":");
                    w.text(uri);
                    w.raw(b",\"mimeType\":");
                    w.text(mime_type);
                    w.raw(b",\"blob\":");
                    w.text(blob);
                    w.raw(b"}}");
                }
            }
        }
        w.raw(b"],\"isError\":");
        w.raw(if result.is_error { b"true" } else { b"false" });
        if let Some(s) = result.structured_content {
            w.raw(b",\"structuredContent\":");
            w.raw(s.raw().as_bytes());
        }
        w.raw(b"}}");
        w.finish()
    }
}
fn base64(s: &str) -> bool {
    if !s.len().is_multiple_of(4) {
        return false;
    }
    let b = s.as_bytes();
    let padding = b.iter().rev().take_while(|&&c| c == b'=').count();
    if padding > 2 {
        return false;
    }
    b[..b.len() - padding]
        .iter()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/'))
}
