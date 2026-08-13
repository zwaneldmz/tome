//! Document conversion read. Per the plan, mammoth/SheetJS move to the
//! renderer (both are browser-capable) — this command becomes a confined
//! `doc_read_bytes`-style byte fetch rather than a main-process conversion,
//! which also gets CVE-prone parsers out of the privileged process. Ports
//! the confinement half of `src/main/index.js`'s `doc:read` handler;
//! `src/main/doc.js:12`'s allowlist stays unchanged.

use crate::ipc::stub_command;

stub_command!(doc_read, "doc:read");
