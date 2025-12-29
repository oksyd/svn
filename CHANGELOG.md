## [0.1.4] - 2025-12-29

### 🐛 Bug Fixes

- Remove doc_auto_cfg for docs.rs
## [0.1.3] - 2025-12-29

### 🚀 Features

- Add IPv6 URLs and harden builders/CI
- Add SessionPool for concurrent sessions
- Add CommitBuilder with svndiff1/2 textdeltas
- Add apply_textdelta for svndiff0/1/2
- Add svndiff0/1/2 textdelta decoder
- Materialize get-file-revs contents via svndiff
- Add fs export, streaming commit, and diff/blame helpers
- Add async editor handler APIs
- Add async filesystem export via TokioFsEditor
- Support copy_from in filesystem export editors
- Configurable session pools

### 🐛 Bug Fixes

- Apply dir copy_from early for correct delta bases
- Harden filesystem export against symlink traversal
- Harden export against reparse point traversal
- Harden export delete and normalize relpaths
- Validate server paths and surface editor errors
- Avoid zero-length svndiff instructions

### ⚡ Performance

- *(ra_svn)* Batch writes and reduce allocations

### ⚙️ Miscellaneous Tasks

- Release svn version 0.1.3
## [0.1.2] - 2025-12-28

### ⚙️ Miscellaneous Tasks

- Add CHANGELOG.md
- *(ci)* Update
- Add LICENSE
- *(docs)* Update README.md
- Release svn version 0.1.2
## [0.1.1] - 2025-12-28

### 🐛 Bug Fixes

- *(auth)* Retry next mechanism after SASL failure
- *(lock)* Normalize LockDesc.path
- *(rasvn)* Encode editor rev as optional tuple

### 🧪 Testing

- *(interop)* Start svnserve with -d
- *(interop)* Use valid property-only commit

### ⚙️ Miscellaneous Tasks

- Init commit
- Update Cargo.toml
- Release svn version 0.1.1
