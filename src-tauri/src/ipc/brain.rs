//! Brain (notes vault) commands: open/close/index/read/write/delete plus
//! core-info and promote. Ports `src/main/brain.js` — frontmatter/wikilinks
//! parsing via `regex` against `test/brain.test.js` fixtures, `notify` +
//! 300ms debounced indexing. Vaults live outside the app config dir (at
//! `~/Tome/Brains`) so gapped agents keep access.

use crate::ipc::stub_command;

stub_command!(brain_open, "brain:open");
stub_command!(brain_close, "brain:close");
stub_command!(brain_index, "brain:index");
stub_command!(brain_read, "brain:read");
stub_command!(brain_write, "brain:write");
stub_command!(brain_delete, "brain:delete");
stub_command!(brain_core_info, "brain:coreInfo");
stub_command!(brain_promote, "brain:promote");
