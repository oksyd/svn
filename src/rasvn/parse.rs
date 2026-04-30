use crate::{
    ChangedPath, CommitInfo, DirEntry, DirListing, FileRev, InheritedProps, LocationEntry,
    LocationSegment, LockDesc, LogEntry, MergeInfoCatalog, NodeKind, PropDelta, PropertyList,
    RepositoryInfo, ServerError, ServerErrorItem, StatEntry, SvnError,
};

use super::SvnItem;

pub(crate) fn parse_proplist(item: &SvnItem) -> Result<PropertyList, SvnError> {
    let entries = item
        .as_list()
        .ok_or_else(|| SvnError::Protocol("proplist not a list".into()))?;
    let mut props = PropertyList::new();
    for entry in entries {
        let items = entry
            .as_list()
            .ok_or_else(|| SvnError::Protocol("proplist entry not a list".into()))?;
        if items.len() < 2 {
            return Err(SvnError::Protocol("proplist entry too short".into()));
        }
        let name = items[0]
            .as_string()
            .ok_or_else(|| SvnError::Protocol("proplist entry name not a string".into()))?;
        let value = items[1]
            .as_bytes_string()
            .ok_or_else(|| SvnError::Protocol("proplist entry value not a string".into()))?;
        props.insert(name, value);
    }
    Ok(props)
}

pub(crate) fn parse_iproplist(item: &SvnItem) -> Result<Vec<InheritedProps>, SvnError> {
    let entries = item
        .as_list()
        .ok_or_else(|| SvnError::Protocol("iproplist not a list".into()))?;
    let mut out = Vec::new();
    for entry in entries {
        let items = entry
            .as_list()
            .ok_or_else(|| SvnError::Protocol("iproplist entry not a list".into()))?;
        if items.len() < 2 {
            return Err(SvnError::Protocol("iproplist entry too short".into()));
        }
        let path = items[0]
            .as_string()
            .ok_or_else(|| SvnError::Protocol("iproplist path not a string".into()))?;
        let props = parse_proplist(&items[1])?;
        out.push(InheritedProps { path, props });
    }
    Ok(out)
}

pub(crate) fn parse_propdelta(item: &SvnItem) -> Result<Vec<PropDelta>, SvnError> {
    let entries = item
        .as_list()
        .ok_or_else(|| SvnError::Protocol("propdelta not a list".into()))?;
    let mut out = Vec::new();
    for entry in entries {
        let items = entry
            .as_list()
            .ok_or_else(|| SvnError::Protocol("propdelta entry not a list".into()))?;
        if items.is_empty() {
            return Err(SvnError::Protocol("propdelta entry too short".into()));
        }
        let name = items[0]
            .as_string()
            .ok_or_else(|| SvnError::Protocol("propdelta name not a string".into()))?;
        let value = match items.get(1) {
            Some(item) => optional_tuple_bytes(item, "propdelta value")?,
            None => None,
        };
        out.push(PropDelta { name, value });
    }
    Ok(out)
}

pub(crate) fn parse_lockdesc(item: &SvnItem) -> Result<LockDesc, SvnError> {
    let items = item
        .as_list()
        .ok_or_else(|| SvnError::Protocol("lockdesc not a list".into()))?;
    if items.len() < 5 {
        return Err(SvnError::Protocol("lockdesc too short".into()));
    }
    let path = items[0]
        .as_string()
        .ok_or_else(|| SvnError::Protocol("lockdesc path not a string".into()))?
        .trim_start_matches('/')
        .to_string();
    let token = items[1]
        .as_string()
        .ok_or_else(|| SvnError::Protocol("lockdesc token not a string".into()))?;
    let owner = items[2]
        .as_string()
        .ok_or_else(|| SvnError::Protocol("lockdesc owner not a string".into()))?;
    let comment = parse_optional_string(items.get(3), "lockdesc comment")?;
    let created = items
        .get(4)
        .and_then(|i| i.as_string())
        .ok_or_else(|| SvnError::Protocol("lockdesc created not a string".into()))?;
    let expires = parse_optional_string(items.get(5), "lockdesc expires")?;

    Ok(LockDesc {
        path,
        token,
        owner,
        comment,
        created,
        expires,
    })
}

pub(crate) fn parse_mergeinfo_catalog(params: &[SvnItem]) -> Result<MergeInfoCatalog, SvnError> {
    let entries = params
        .first()
        .and_then(|i| i.as_list())
        .ok_or_else(|| SvnError::Protocol("mergeinfo response not a list".into()))?;
    let mut out = MergeInfoCatalog::new();
    for entry in entries {
        let items = entry
            .as_list()
            .ok_or_else(|| SvnError::Protocol("mergeinfo entry not a list".into()))?;
        if items.len() < 2 {
            return Err(SvnError::Protocol("mergeinfo entry too short".into()));
        }
        let path = items[0]
            .as_string()
            .ok_or_else(|| SvnError::Protocol("mergeinfo path not a string".into()))?;
        let mergeinfo = items[1]
            .as_string()
            .ok_or_else(|| SvnError::Protocol("mergeinfo value not a string".into()))?;
        out.insert(path.trim_start_matches('/').to_string(), mergeinfo);
    }
    Ok(out)
}

pub(crate) fn parse_word_list(item: &SvnItem, ctx: &str) -> Result<Vec<String>, SvnError> {
    let items = item
        .as_list()
        .ok_or_else(|| SvnError::Protocol(format!("{ctx} not a list")))?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let word = item
            .as_word()
            .ok_or_else(|| SvnError::Protocol(format!("{ctx} entry not a word")))?;
        out.push(word);
    }
    Ok(out)
}

pub(crate) fn parse_location_entry(item: SvnItem) -> Result<LocationEntry, SvnError> {
    let items = item
        .as_list()
        .ok_or_else(|| SvnError::Protocol("location entry not a list".into()))?;
    if items.len() < 2 {
        return Err(SvnError::Protocol("location entry too short".into()));
    }
    let rev = items[0]
        .as_u64()
        .ok_or_else(|| SvnError::Protocol("location entry rev not a number".into()))?;
    let path = items[1]
        .as_string()
        .ok_or_else(|| SvnError::Protocol("location entry path not a string".into()))?;
    Ok(LocationEntry {
        rev,
        path: path.trim_start_matches('/').to_string(),
    })
}

pub(crate) fn parse_location_segment(item: SvnItem) -> Result<LocationSegment, SvnError> {
    let items = item
        .as_list()
        .ok_or_else(|| SvnError::Protocol("location segment not a list".into()))?;
    if items.len() < 2 {
        return Err(SvnError::Protocol("location segment too short".into()));
    }
    let range_start = items[0]
        .as_u64()
        .ok_or_else(|| SvnError::Protocol("location segment start not a number".into()))?;
    let range_end = items[1]
        .as_u64()
        .ok_or_else(|| SvnError::Protocol("location segment end not a number".into()))?;
    let path = parse_optional_string(items.get(2), "location segment path")?
        .map(|path| path.trim_start_matches('/').to_string());
    Ok(LocationSegment {
        range_start,
        range_end,
        path,
    })
}

pub(crate) fn parse_file_rev_entry(item: SvnItem) -> Result<FileRev, SvnError> {
    let items = item
        .as_list()
        .ok_or_else(|| SvnError::Protocol("file-rev entry not a list".into()))?;
    if items.len() < 4 {
        return Err(SvnError::Protocol("file-rev entry too short".into()));
    }
    let path = items[0]
        .as_string()
        .ok_or_else(|| SvnError::Protocol("file-rev path not a string".into()))?
        .trim_start_matches('/')
        .to_string();
    let rev = items[1]
        .as_u64()
        .ok_or_else(|| SvnError::Protocol("file-rev rev not a number".into()))?;
    let rev_props = parse_proplist(&items[2])?;
    let prop_deltas = parse_propdelta(&items[3])?;
    let merged_revision =
        parse_optional_bool(items.get(4), "file-rev merged-revision")?.unwrap_or(false);
    Ok(FileRev {
        path,
        rev,
        rev_props,
        prop_deltas,
        merged_revision,
        delta_chunks: Vec::new(),
    })
}

pub(crate) fn parse_repos_info(params: &[SvnItem]) -> Result<RepositoryInfo, SvnError> {
    if params.is_empty() {
        return Err(SvnError::Protocol("repos-info params empty".into()));
    }
    let uuid = params[0]
        .as_string()
        .ok_or_else(|| SvnError::Protocol("repos-info uuid not a string".into()))?;
    let root_url = match params.get(1) {
        Some(item) => item
            .as_string()
            .ok_or_else(|| SvnError::Protocol("repos-info root url not a string".into()))?,
        None => String::new(),
    };
    let capabilities = match params.get(2) {
        Some(item) => parse_word_list(item, "repos-info caps")?,
        None => Vec::new(),
    };
    Ok(RepositoryInfo {
        uuid,
        root_url,
        capabilities,
    })
}

pub(crate) fn parse_server_error(items: &[SvnItem]) -> ServerError {
    let mut chain = Vec::new();
    for item in items {
        let SvnItem::List(parts) = item else {
            continue;
        };
        if parts.len() < 4 {
            continue;
        }

        let code = parts[0].as_u64().unwrap_or(0);
        let message = lossy_string(&parts[1]);
        let message = message.filter(|m| !m.is_empty());
        let file = lossy_string(&parts[2]).filter(|s| !s.is_empty());
        let line = parts[3].as_u64();

        chain.push(ServerErrorItem {
            code,
            message,
            file,
            line,
        });
    }

    ServerError {
        context: None,
        chain,
    }
}

fn lossy_string(item: &SvnItem) -> Option<String> {
    match item {
        SvnItem::String(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        SvnItem::Word(word) => Some(word.clone()),
        _ => item.as_string(),
    }
}

pub(crate) fn parse_commit_info(item: &SvnItem) -> Result<CommitInfo, SvnError> {
    let items = item
        .as_list()
        .ok_or_else(|| SvnError::Protocol("commit-info not a list".into()))?;
    if items.is_empty() {
        return Err(SvnError::Protocol("commit-info too short".into()));
    }
    let new_rev = items[0]
        .as_u64()
        .ok_or_else(|| SvnError::Protocol("commit-info new-rev not a number".into()))?;
    let date = parse_optional_string(items.get(1), "commit-info date")?;
    let author = parse_optional_string(items.get(2), "commit-info author")?;
    let post_commit_err = parse_optional_string(items.get(3), "commit-info post-commit error")?;

    Ok(CommitInfo {
        new_rev,
        date,
        author,
        post_commit_err,
    })
}

pub(crate) struct GetFileResponseParams {
    pub(crate) checksum: Option<String>,
    pub(crate) rev: u64,
    pub(crate) props: PropertyList,
    pub(crate) inherited_props: Vec<InheritedProps>,
}

pub(crate) fn parse_get_file_response_params(
    params: &[SvnItem],
) -> Result<GetFileResponseParams, SvnError> {
    if params.len() < 3 {
        return Err(SvnError::Protocol("get-file response too short".into()));
    }

    let checksum = parse_optional_string(params.first(), "get-file checksum")?;
    let rev = params[1]
        .as_u64()
        .ok_or_else(|| SvnError::Protocol("get-file rev not a number".into()))?;
    let props = parse_proplist(&params[2])?;
    let inherited_props = match params.get(3) {
        None => Vec::new(),
        Some(item) => match item {
            SvnItem::List(items) if items.is_empty() => Vec::new(),
            SvnItem::List(items) if items.len() == 1 => parse_iproplist(&items[0])?,
            SvnItem::List(_) => {
                return Err(SvnError::Protocol("get-file iprops tuple too long".into()));
            }
            _ => return Err(SvnError::Protocol("get-file iprops not a list".into())),
        },
    };

    Ok(GetFileResponseParams {
        checksum,
        rev,
        props,
        inherited_props,
    })
}

pub(crate) fn parse_get_dir_listing(
    dir_path: &str,
    params: &[SvnItem],
) -> Result<DirListing, SvnError> {
    if params.len() < 3 {
        return Err(SvnError::Protocol("get-dir response too short".into()));
    }

    let listing_rev = params[0]
        .as_u64()
        .ok_or_else(|| SvnError::Protocol("get-dir rev not a number".into()))?;
    let entries_list = params[2]
        .as_list()
        .ok_or_else(|| SvnError::Protocol("get-dir entries not a list".into()))?;

    let mut entries = Vec::new();
    let dir_prefix = dir_path.trim_end_matches('/');
    for entry in entries_list {
        let items = entry
            .as_list()
            .ok_or_else(|| SvnError::Protocol("get-dir entry not a list".into()))?;
        if items.len() < 2 {
            return Err(SvnError::Protocol("get-dir entry too short".into()));
        }
        let name = items[0]
            .as_string()
            .ok_or_else(|| SvnError::Protocol("get-dir entry name not a string".into()))?;

        let kind_word = parse_optional_wordish(items.get(1), "get-dir entry kind")?
            .ok_or_else(|| SvnError::Protocol("get-dir entry kind not a word".into()))?;
        let kind = NodeKind::from_word(&kind_word);

        let size = parse_optional_u64(items.get(2), "get-dir entry size")?;
        let has_props = parse_optional_bool(items.get(3), "get-dir entry has-props")?;
        let created_rev = parse_optional_u64(items.get(4), "get-dir entry created-rev")?;
        let created_date = parse_optional_string(items.get(5), "get-dir entry created-date")?;
        let last_author = parse_optional_string(items.get(6), "get-dir entry last-author")?;

        let full_path = if dir_prefix.is_empty() {
            name.clone()
        } else {
            format!("{dir_prefix}/{name}")
        };

        entries.push(DirEntry {
            name,
            path: full_path,
            kind,
            size,
            has_props,
            created_rev,
            created_date,
            last_author,
        });
    }

    Ok(DirListing {
        rev: listing_rev,
        entries,
    })
}

pub(crate) fn parse_log_entry(
    items: Vec<SvnItem>,
    expect_custom_revprops: bool,
) -> Result<LogEntry, SvnError> {
    if items.len() < 2 {
        return Err(SvnError::Protocol("log entry too short".into()));
    }
    let changes = items[0]
        .as_list()
        .ok_or_else(|| SvnError::Protocol("log entry changes not a list".into()))?;
    let rev = items[1]
        .as_u64()
        .ok_or_else(|| SvnError::Protocol("log entry rev not a number".into()))?;

    let author = parse_optional_string(items.get(2), "log author")?;
    let date = parse_optional_string(items.get(3), "log date")?;
    let message = parse_optional_string(items.get(4), "log message")?;

    let mut has_children = false;
    let mut invalid_revnum = false;
    let mut rev_props = PropertyList::new();

    let mut idx = 5;
    if let (Some(hc), Some(invalid)) = (
        items.get(idx).and_then(opt_tuple_bool),
        items.get(idx + 1).and_then(opt_tuple_bool),
    ) {
        has_children = hc;
        invalid_revnum = invalid;
        idx += 2;
    }

    let mut saw_revprops_block = false;
    if items.get(idx).and_then(opt_tuple_u64).is_some()
        && let Some(props_item) = items.get(idx + 1)
    {
        rev_props = parse_proplist(props_item)?;
        saw_revprops_block = true;
        idx += 2;
    }
    if expect_custom_revprops && !saw_revprops_block {
        return Err(SvnError::Protocol(
            "server does not support custom revprops via log".into(),
        ));
    }
    let subtractive_merge =
        parse_optional_bool(items.get(idx), "log subtractive-merge")?.unwrap_or(false);

    let mut changed_paths = Vec::new();
    for change in changes {
        let change_items = change
            .as_list()
            .ok_or_else(|| SvnError::Protocol("log changed-path not a list".into()))?;
        if change_items.len() < 2 {
            return Err(SvnError::Protocol("log changed-path too short".into()));
        };
        let path = change_items[0]
            .as_string()
            .ok_or_else(|| SvnError::Protocol("log changed-path path not a string".into()))?;
        let action = change_items[1]
            .as_word()
            .ok_or_else(|| SvnError::Protocol("log changed-path action not a word".into()))?;

        let (copy_from_path, copy_from_rev) = parse_log_copyfrom(change_items.get(2))?;

        let node_flags = parse_log_node_flags(change_items.get(3))?;

        changed_paths.push(ChangedPath {
            action,
            path: path.trim_start_matches('/').to_string(),
            copy_from_path,
            copy_from_rev,
            node_kind: node_flags.kind,
            text_mods: node_flags.text_mods,
            prop_mods: node_flags.prop_mods,
        });
    }
    Ok(LogEntry {
        rev,
        changed_paths,
        author,
        date,
        message,
        rev_props,
        has_children,
        invalid_revnum,
        subtractive_merge,
    })
}

pub(crate) fn parse_list_dirent(items: Vec<SvnItem>) -> Result<DirEntry, SvnError> {
    if items.len() < 2 {
        return Err(SvnError::Protocol("list dirent too short".into()));
    }

    let rel_path = items[0]
        .as_string()
        .ok_or_else(|| SvnError::Protocol("list dirent path missing".into()))?;
    let kind_word = parse_optional_wordish(items.get(1), "list dirent kind")?
        .ok_or_else(|| SvnError::Protocol("list dirent kind not a word".into()))?;
    let kind = NodeKind::from_word(&kind_word);

    let size = parse_optional_u64(items.get(2), "list dirent size")?;
    let has_props = parse_optional_bool(items.get(3), "list dirent has-props")?;
    let created_rev = parse_optional_u64(items.get(4), "list dirent created-rev")?;
    let created_date = parse_optional_string(items.get(5), "list dirent created-date")?;
    let last_author = parse_optional_string(items.get(6), "list dirent last-author")?;

    let rel_path = rel_path.trim_start_matches('/').to_string();
    let name = rel_path
        .rsplit_once('/')
        .map(|(_, name)| name.to_string())
        .unwrap_or_else(|| rel_path.clone());

    Ok(DirEntry {
        name,
        path: rel_path,
        kind,
        size,
        has_props,
        created_rev,
        created_date,
        last_author,
    })
}

pub(crate) fn parse_stat_params(params: &[SvnItem]) -> Result<Option<StatEntry>, SvnError> {
    if params.is_empty() {
        return Ok(None);
    }

    if params.len() == 1
        && let Some(items) = params[0].as_list()
        && let Some(entry) = parse_stat_entry(&items)?
    {
        return Ok(Some(entry));
    }

    if params.len() >= 2
        && opt_tuple_u64(&params[0]).is_some()
        && let Some(items) = params[1].as_list()
        && let Some(entry) = parse_stat_entry(&items)?
    {
        return Ok(Some(entry));
    }

    if let Some(entry) = parse_stat_entry(params)? {
        return Ok(Some(entry));
    }

    for item in params {
        if let Some(items) = item.as_list()
            && let Some(entry) = parse_stat_entry(&items)?
        {
            return Ok(Some(entry));
        }
    }

    Ok(None)
}

fn parse_stat_entry(items: &[SvnItem]) -> Result<Option<StatEntry>, SvnError> {
    if items.len() >= 2
        && opt_tuple_u64(&items[0]).is_some()
        && let Some(entry) = parse_stat_entry_at(items, 1)?
    {
        return Ok(Some(entry));
    }
    if let Some(entry) = parse_stat_entry_at(items, 0)? {
        return Ok(Some(entry));
    }
    Ok(None)
}

fn parse_stat_entry_at(items: &[SvnItem], offset: usize) -> Result<Option<StatEntry>, SvnError> {
    let Some(kind_word) = items.get(offset).and_then(opt_tuple_wordish) else {
        return Ok(None);
    };
    let kind = NodeKind::from_word(&kind_word);
    if matches!(kind, NodeKind::Unknown | NodeKind::None) {
        return Ok(None);
    }

    let size = parse_optional_u64(items.get(offset + 1), "stat size")?;
    let has_props = parse_optional_bool(items.get(offset + 2), "stat has-props")?;
    let created_rev = parse_optional_u64(items.get(offset + 3), "stat created-rev")?;
    let created_date = parse_optional_string(items.get(offset + 4), "stat created-date")?;
    let last_author = parse_optional_string(items.get(offset + 5), "stat last-author")?;

    Ok(Some(StatEntry {
        kind,
        size,
        has_props,
        created_rev,
        created_date,
        last_author,
    }))
}

fn parse_optional_string(item: Option<&SvnItem>, ctx: &str) -> Result<Option<String>, SvnError> {
    parse_optional_scalar(item, ctx, "a string", SvnItem::as_string)
}

fn parse_optional_u64(item: Option<&SvnItem>, ctx: &str) -> Result<Option<u64>, SvnError> {
    parse_optional_scalar(item, ctx, "a number", SvnItem::as_u64)
}

fn parse_optional_bool(item: Option<&SvnItem>, ctx: &str) -> Result<Option<bool>, SvnError> {
    parse_optional_scalar(item, ctx, "a bool", SvnItem::as_bool)
}

fn parse_optional_wordish(item: Option<&SvnItem>, ctx: &str) -> Result<Option<String>, SvnError> {
    parse_optional_scalar(item, ctx, "a word", opt_tuple_wordish)
}

fn parse_optional_scalar<T>(
    item: Option<&SvnItem>,
    ctx: &str,
    expected: &str,
    parse: impl Fn(&SvnItem) -> Option<T>,
) -> Result<Option<T>, SvnError> {
    let Some(item) = item else {
        return Ok(None);
    };
    match item {
        SvnItem::List(items) if items.is_empty() => Ok(None),
        SvnItem::List(items) if items.len() == 1 => parse(&items[0])
            .map(Some)
            .ok_or_else(|| SvnError::Protocol(format!("{ctx} not {expected}"))),
        SvnItem::List(_) => Err(SvnError::Protocol(format!("{ctx} tuple too long"))),
        _ => parse(item)
            .map(Some)
            .ok_or_else(|| SvnError::Protocol(format!("{ctx} not {expected}"))),
    }
}

fn optional_tuple_bytes(item: &SvnItem, ctx: &str) -> Result<Option<Vec<u8>>, SvnError> {
    match item {
        SvnItem::List(items) if items.is_empty() => Ok(None),
        SvnItem::List(items) if items.len() == 1 => items[0]
            .as_bytes_string()
            .map(Some)
            .ok_or_else(|| SvnError::Protocol(format!("{ctx} not a string"))),
        SvnItem::List(_) => Err(SvnError::Protocol(format!("{ctx} tuple too long"))),
        _ => item
            .as_bytes_string()
            .map(Some)
            .ok_or_else(|| SvnError::Protocol(format!("{ctx} not a string"))),
    }
}

fn parse_log_copyfrom(item: Option<&SvnItem>) -> Result<(Option<String>, Option<u64>), SvnError> {
    let Some(item) = item else {
        return Ok((None, None));
    };
    let items = item
        .as_list()
        .ok_or_else(|| SvnError::Protocol("log copy-from not a tuple".into()))?;
    if items.is_empty() {
        return Ok((None, None));
    }
    if items.len() != 2 {
        return Err(SvnError::Protocol(
            "log copy-from tuple must contain path and rev".into(),
        ));
    }
    let path = items[0]
        .as_string()
        .ok_or_else(|| SvnError::Protocol("log copy-from path not a string".into()))?
        .trim_start_matches('/')
        .to_string();
    let rev = items[1]
        .as_u64()
        .ok_or_else(|| SvnError::Protocol("log copy-from rev not a number".into()))?;
    Ok((Some(path), Some(rev)))
}

struct LogNodeFlags {
    kind: Option<NodeKind>,
    text_mods: Option<bool>,
    prop_mods: Option<bool>,
}

impl LogNodeFlags {
    fn empty() -> Self {
        Self {
            kind: None,
            text_mods: None,
            prop_mods: None,
        }
    }
}

fn parse_log_node_flags(item: Option<&SvnItem>) -> Result<LogNodeFlags, SvnError> {
    let Some(item) = item else {
        return Ok(LogNodeFlags::empty());
    };
    let items = item
        .as_list()
        .ok_or_else(|| SvnError::Protocol("log node flags not a tuple".into()))?;
    if items.is_empty() {
        return Ok(LogNodeFlags::empty());
    }

    let kind = items
        .first()
        .map(|item| {
            item.as_string()
                .or_else(|| item.as_word())
                .map(|word| NodeKind::from_word(&word))
                .ok_or_else(|| SvnError::Protocol("log node kind not a word".into()))
        })
        .transpose()?;
    let text_mods = match items.get(1) {
        Some(item) => Some(
            item.as_bool()
                .ok_or_else(|| SvnError::Protocol("log text-mods not a bool".into()))?,
        ),
        None => None,
    };
    let prop_mods = match items.get(2) {
        Some(item) => Some(
            item.as_bool()
                .ok_or_else(|| SvnError::Protocol("log prop-mods not a bool".into()))?,
        ),
        None => None,
    };
    Ok(LogNodeFlags {
        kind,
        text_mods,
        prop_mods,
    })
}

pub(crate) fn opt_tuple_wordish(item: &SvnItem) -> Option<String> {
    match item {
        SvnItem::List(items) => items.first().and_then(opt_tuple_wordish),
        SvnItem::Word(_) => item.as_word(),
        SvnItem::String(_) => item.as_string(),
        _ => None,
    }
}

fn opt_tuple_u64(item: &SvnItem) -> Option<u64> {
    match item {
        SvnItem::List(items) => items.first().and_then(|i| i.as_u64()),
        _ => item.as_u64(),
    }
}

fn opt_tuple_bool(item: &SvnItem) -> Option<bool> {
    match item {
        SvnItem::List(items) => items.first().and_then(|i| i.as_bool()),
        _ => item.as_bool(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn parse_log_entry_extracts_author_and_copyfrom() {
        let change = SvnItem::List(vec![
            SvnItem::String(b"/trunk/a.zip".to_vec()),
            SvnItem::Word("A".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"/branches/b1/a.zip".to_vec()),
                SvnItem::Number(9),
            ]),
            SvnItem::List(vec![
                SvnItem::String(b"file".to_vec()),
                SvnItem::Bool(true),
                SvnItem::Bool(false),
            ]),
        ]);

        let items = vec![
            SvnItem::List(vec![change]),
            SvnItem::Number(10),
            SvnItem::List(vec![SvnItem::String(b"alice".to_vec())]),
            SvnItem::List(vec![SvnItem::String(b"2025-01-01".to_vec())]),
            SvnItem::List(vec![SvnItem::String(b"msg".to_vec())]),
        ];

        let entry = parse_log_entry(items, false).unwrap();
        assert_eq!(entry.rev, 10);
        assert_eq!(entry.author.as_deref(), Some("alice"));
        assert_eq!(entry.date.as_deref(), Some("2025-01-01"));
        assert_eq!(entry.message.as_deref(), Some("msg"));
        assert_eq!(entry.changed_paths.len(), 1);
        assert!(entry.rev_props.is_empty());
        assert!(!entry.has_children);
        assert!(!entry.invalid_revnum);
        assert!(!entry.subtractive_merge);

        let change = &entry.changed_paths[0];
        assert_eq!(change.action, "A");
        assert_eq!(change.path, "trunk/a.zip");
        assert_eq!(change.copy_from_path.as_deref(), Some("branches/b1/a.zip"));
        assert_eq!(change.copy_from_rev, Some(9));
        assert_eq!(change.node_kind, Some(NodeKind::File));
        assert_eq!(change.text_mods, Some(true));
        assert_eq!(change.prop_mods, Some(false));
    }

    #[test]
    fn parse_log_entry_handles_missing_optional_parts() {
        let change = SvnItem::List(vec![
            SvnItem::String(b"/trunk/a.zip".to_vec()),
            SvnItem::Word("M".to_string()),
        ]);

        let items = vec![SvnItem::List(vec![change]), SvnItem::Number(10)];
        let entry = parse_log_entry(items, false).unwrap();
        assert_eq!(entry.author, None);
        assert_eq!(entry.changed_paths.len(), 1);
        assert_eq!(entry.changed_paths[0].copy_from_path, None);
        assert_eq!(entry.changed_paths[0].node_kind, None);
        assert!(entry.rev_props.is_empty());
        assert!(!entry.has_children);
        assert!(!entry.invalid_revnum);
        assert!(!entry.subtractive_merge);
    }

    #[test]
    fn parse_log_entry_rejects_invalid_shapes() {
        let err = parse_log_entry(Vec::new(), false).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));

        let err = parse_log_entry(
            vec![SvnItem::Word("x".to_string()), SvnItem::Number(1)],
            false,
        )
        .unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));

        let err = parse_log_entry(
            vec![SvnItem::List(Vec::new()), SvnItem::Word("x".to_string())],
            false,
        )
        .unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));

        let change = SvnItem::Word("bad-change".to_string());
        let err = parse_log_entry(
            vec![SvnItem::List(vec![change]), SvnItem::Number(10)],
            false,
        )
        .unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "log changed-path not a list"));

        let change = SvnItem::List(vec![SvnItem::String(b"/trunk/a.zip".to_vec())]);
        let err = parse_log_entry(
            vec![SvnItem::List(vec![change]), SvnItem::Number(10)],
            false,
        )
        .unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "log changed-path too short"));
    }

    #[test]
    fn parse_log_entry_rejects_malformed_copyfrom() {
        let change = SvnItem::List(vec![
            SvnItem::String(b"/trunk/a.zip".to_vec()),
            SvnItem::Word("A".to_string()),
            SvnItem::Number(1),
        ]);
        let err = parse_log_entry(
            vec![SvnItem::List(vec![change]), SvnItem::Number(10)],
            false,
        )
        .unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "log copy-from not a tuple"));

        let change = SvnItem::List(vec![
            SvnItem::String(b"/trunk/a.zip".to_vec()),
            SvnItem::Word("A".to_string()),
            SvnItem::List(vec![SvnItem::String(b"/branches/a.zip".to_vec())]),
        ]);
        let err = parse_log_entry(
            vec![SvnItem::List(vec![change]), SvnItem::Number(10)],
            false,
        )
        .unwrap_err();
        assert!(
            matches!(err, SvnError::Protocol(msg) if msg == "log copy-from tuple must contain path and rev")
        );
    }

    #[test]
    fn parse_log_entry_rejects_malformed_node_flags() {
        let change = SvnItem::List(vec![
            SvnItem::String(b"/trunk/a.zip".to_vec()),
            SvnItem::Word("M".to_string()),
            SvnItem::List(Vec::new()),
            SvnItem::Number(1),
        ]);
        let err = parse_log_entry(
            vec![SvnItem::List(vec![change]), SvnItem::Number(10)],
            false,
        )
        .unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "log node flags not a tuple"));

        let change = SvnItem::List(vec![
            SvnItem::String(b"/trunk/a.zip".to_vec()),
            SvnItem::Word("M".to_string()),
            SvnItem::List(Vec::new()),
            SvnItem::List(vec![SvnItem::Word("file".to_string()), SvnItem::Number(1)]),
        ]);
        let err = parse_log_entry(
            vec![SvnItem::List(vec![change]), SvnItem::Number(10)],
            false,
        )
        .unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "log text-mods not a bool"));
    }

    #[test]
    fn parse_log_entry_parses_revprops_and_merge_flags() {
        let items = vec![
            SvnItem::List(Vec::new()),
            SvnItem::Number(10),
            SvnItem::List(vec![SvnItem::String(b"alice".to_vec())]),
            SvnItem::List(vec![SvnItem::String(b"2025-01-01".to_vec())]),
            SvnItem::List(vec![SvnItem::String(b"msg".to_vec())]),
            SvnItem::Bool(true),
            SvnItem::Bool(false),
            SvnItem::Number(1),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::String(b"svn:custom".to_vec()),
                SvnItem::String(b"x".to_vec()),
            ])]),
            SvnItem::Bool(true),
        ];

        let entry = parse_log_entry(items, true).unwrap();
        assert_eq!(entry.rev, 10);
        assert_eq!(entry.author.as_deref(), Some("alice"));
        assert_eq!(entry.date.as_deref(), Some("2025-01-01"));
        assert_eq!(entry.message.as_deref(), Some("msg"));
        assert!(entry.has_children);
        assert!(!entry.invalid_revnum);
        assert!(entry.subtractive_merge);
        assert_eq!(entry.rev_props.get("svn:custom").unwrap(), b"x");
    }

    #[test]
    fn parse_log_entry_errors_when_custom_revprops_missing() {
        let items = vec![SvnItem::List(Vec::new()), SvnItem::Number(10)];
        let err = parse_log_entry(items, true).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg.contains("custom revprops")));
    }

    #[test]
    fn parse_list_dirent_parses_optional_tuple_fields() {
        let items = vec![
            SvnItem::String(b"/trunk/a.zip".to_vec()),
            SvnItem::Word("file".to_string()),
            SvnItem::List(vec![SvnItem::Number(10)]),
            SvnItem::List(vec![SvnItem::Bool(true)]),
            SvnItem::List(vec![SvnItem::Number(5)]),
            SvnItem::List(vec![SvnItem::String(b"2025-01-01".to_vec())]),
            SvnItem::List(vec![SvnItem::String(b"alice".to_vec())]),
        ];

        let entry = parse_list_dirent(items).unwrap();
        assert_eq!(entry.path, "trunk/a.zip");
        assert_eq!(entry.name, "a.zip");
        assert_eq!(entry.kind, NodeKind::File);
        assert_eq!(entry.size, Some(10));
        assert_eq!(entry.has_props, Some(true));
        assert_eq!(entry.created_rev, Some(5));
        assert_eq!(entry.created_date.as_deref(), Some("2025-01-01"));
        assert_eq!(entry.last_author.as_deref(), Some("alice"));
    }

    #[test]
    fn parse_list_dirent_extracts_name_from_nested_paths() {
        let items = vec![
            SvnItem::String(b"/trunk/dir/a.zip".to_vec()),
            SvnItem::Word("file".to_string()),
        ];

        let entry = parse_list_dirent(items).unwrap();
        assert_eq!(entry.path, "trunk/dir/a.zip");
        assert_eq!(entry.name, "a.zip");
    }

    #[test]
    fn parse_list_dirent_rejects_short_response() {
        let err = parse_list_dirent(Vec::new()).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));
    }

    #[test]
    fn parse_list_dirent_rejects_malformed_optional_fields() {
        let items = vec![
            SvnItem::String(b"/trunk/a.zip".to_vec()),
            SvnItem::Number(1),
        ];
        let err = parse_list_dirent(items).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "list dirent kind not a word"));

        let items = vec![
            SvnItem::String(b"/trunk/a.zip".to_vec()),
            SvnItem::Word("file".to_string()),
            SvnItem::Word("large".to_string()),
        ];
        let err = parse_list_dirent(items).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "list dirent size not a number"));
    }

    #[test]
    fn parse_get_dir_listing_reads_optional_tuple_strings() {
        let params = vec![
            SvnItem::Number(42),
            SvnItem::List(Vec::new()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::String(b"a.txt".to_vec()),
                SvnItem::Word("file".to_string()),
                SvnItem::Number(10),
                SvnItem::Bool(false),
                SvnItem::Number(1),
                SvnItem::List(vec![SvnItem::String(b"2025-01-01".to_vec())]),
                SvnItem::List(vec![SvnItem::String(b"bob".to_vec())]),
            ])]),
        ];

        let listing = parse_get_dir_listing("trunk", &params).unwrap();
        assert_eq!(listing.rev, 42);
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].path, "trunk/a.txt");
        assert_eq!(
            listing.entries[0].created_date.as_deref(),
            Some("2025-01-01")
        );
        assert_eq!(listing.entries[0].last_author.as_deref(), Some("bob"));
    }

    #[test]
    fn parse_get_dir_listing_trims_trailing_slash_and_handles_root() {
        let params = vec![
            SvnItem::Number(1),
            SvnItem::List(Vec::new()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::String(b"a.txt".to_vec()),
                SvnItem::Word("file".to_string()),
            ])]),
        ];

        let listing = parse_get_dir_listing("trunk/", &params).unwrap();
        assert_eq!(listing.entries[0].path, "trunk/a.txt");

        let listing = parse_get_dir_listing("", &params).unwrap();
        assert_eq!(listing.entries[0].path, "a.txt");
    }

    #[test]
    fn parse_get_dir_listing_rejects_malformed_entries() {
        let params = vec![
            SvnItem::Number(1),
            SvnItem::List(Vec::new()),
            SvnItem::List(vec![
                SvnItem::Word("junk".to_string()),
                SvnItem::List(Vec::new()),
                SvnItem::List(vec![
                    SvnItem::String(b"a.txt".to_vec()),
                    SvnItem::Word("file".to_string()),
                ]),
            ]),
        ];

        let err = parse_get_dir_listing("trunk", &params).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "get-dir entry not a list"));

        let params = vec![
            SvnItem::Number(1),
            SvnItem::List(Vec::new()),
            SvnItem::List(vec![SvnItem::List(vec![SvnItem::String(
                b"a.txt".to_vec(),
            )])]),
        ];
        let err = parse_get_dir_listing("trunk", &params).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "get-dir entry too short"));

        let params = vec![
            SvnItem::Number(1),
            SvnItem::List(Vec::new()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::String(b"a.txt".to_vec()),
                SvnItem::Word("file".to_string()),
                SvnItem::Word("large".to_string()),
            ])]),
        ];
        let err = parse_get_dir_listing("trunk", &params).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "get-dir entry size not a number"));
    }

    #[test]
    fn parse_get_dir_listing_rejects_invalid_shapes() {
        let err = parse_get_dir_listing("trunk", &[]).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));

        let params = vec![
            SvnItem::Word("x".to_string()),
            SvnItem::List(Vec::new()),
            SvnItem::List(Vec::new()),
        ];
        let err = parse_get_dir_listing("trunk", &params).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));

        let params = vec![
            SvnItem::Number(1),
            SvnItem::List(Vec::new()),
            SvnItem::Word("x".to_string()),
        ];
        let err = parse_get_dir_listing("trunk", &params).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));
    }

    #[test]
    fn parse_stat_params_supports_multiple_layouts() {
        let entry_items = vec![
            SvnItem::List(vec![SvnItem::Word("file".to_string())]),
            SvnItem::List(vec![SvnItem::Number(10)]),
            SvnItem::List(vec![SvnItem::Bool(true)]),
            SvnItem::List(vec![SvnItem::Number(5)]),
            SvnItem::List(vec![SvnItem::String(b"2025-01-01".to_vec())]),
            SvnItem::List(vec![SvnItem::String(b"alice".to_vec())]),
        ];

        let entry = parse_stat_params(&entry_items).unwrap().unwrap();
        assert_eq!(entry.kind, NodeKind::File);
        assert_eq!(entry.size, Some(10));
        assert_eq!(entry.has_props, Some(true));
        assert_eq!(entry.created_rev, Some(5));
        assert_eq!(entry.created_date.as_deref(), Some("2025-01-01"));
        assert_eq!(entry.last_author.as_deref(), Some("alice"));

        let params = vec![SvnItem::List(entry_items.clone())];
        assert!(parse_stat_params(&params).unwrap().is_some());

        let params = vec![
            SvnItem::List(vec![SvnItem::Number(123)]),
            SvnItem::List(entry_items.clone()),
        ];
        assert!(parse_stat_params(&params).unwrap().is_some());

        let params = vec![
            SvnItem::Word("junk".to_string()),
            SvnItem::List(entry_items),
        ];
        assert!(parse_stat_params(&params).unwrap().is_some());
    }

    #[test]
    fn parse_stat_params_returns_none_for_unknown_or_none_kind() {
        let params = vec![SvnItem::Word("none".to_string())];
        assert!(parse_stat_params(&params).unwrap().is_none());
        let params = vec![SvnItem::Word("wat".to_string())];
        assert!(parse_stat_params(&params).unwrap().is_none());
    }

    #[test]
    fn parse_stat_params_rejects_malformed_fields_after_known_kind() {
        let params = vec![
            SvnItem::Word("file".to_string()),
            SvnItem::Word("large".to_string()),
        ];
        let err = parse_stat_params(&params).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "stat size not a number"));
    }

    #[test]
    fn parse_mergeinfo_catalog_rejects_malformed_entries() {
        let params = vec![SvnItem::List(vec![SvnItem::Number(1)])];
        let err = parse_mergeinfo_catalog(&params).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "mergeinfo entry not a list"));

        let params = vec![SvnItem::List(vec![SvnItem::List(vec![SvnItem::String(
            b"/trunk".to_vec(),
        )])])];
        let err = parse_mergeinfo_catalog(&params).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "mergeinfo entry too short"));
    }

    #[test]
    fn parse_proplist_reads_binary_values() {
        let props_item = SvnItem::List(vec![
            SvnItem::List(vec![
                SvnItem::String(b"svn:mime-type".to_vec()),
                SvnItem::String(b"text/plain".to_vec()),
            ]),
            SvnItem::List(vec![
                SvnItem::String(b"svn:binary".to_vec()),
                SvnItem::String(vec![0, 1, 2, 3]),
            ]),
        ]);

        let props = parse_proplist(&props_item).unwrap();
        assert_eq!(props.get("svn:mime-type").unwrap(), b"text/plain");
        assert_eq!(props.get("svn:binary").unwrap(), &[0, 1, 2, 3]);
    }

    #[test]
    fn parse_proplist_rejects_malformed_entries() {
        let props_item = SvnItem::List(vec![SvnItem::List(vec![SvnItem::String(
            b"missing-value".to_vec(),
        )])]);
        let err = parse_proplist(&props_item).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "proplist entry too short"));

        let props_item = SvnItem::List(vec![SvnItem::Number(1)]);
        let err = parse_proplist(&props_item).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "proplist entry not a list"));
    }

    #[test]
    fn parse_propdelta_rejects_malformed_entries() {
        let deltas = SvnItem::List(vec![SvnItem::List(Vec::new())]);
        let err = parse_propdelta(&deltas).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "propdelta entry too short"));

        let deltas = SvnItem::List(vec![SvnItem::Number(1)]);
        let err = parse_propdelta(&deltas).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "propdelta entry not a list"));

        let deltas = SvnItem::List(vec![SvnItem::List(vec![
            SvnItem::String(b"p".to_vec()),
            SvnItem::List(vec![SvnItem::Number(1)]),
        ])]);
        let err = parse_propdelta(&deltas).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "propdelta value not a string"));
    }

    #[test]
    fn parse_iproplist_reads_inherited_entries() {
        let props_item = SvnItem::List(vec![SvnItem::List(vec![
            SvnItem::String(b"p".to_vec()),
            SvnItem::String(b"v".to_vec()),
        ])]);
        let iprops_item = SvnItem::List(vec![SvnItem::List(vec![
            SvnItem::String(b"/trunk".to_vec()),
            props_item,
        ])]);

        let parsed = parse_iproplist(&iprops_item).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].path, "/trunk");
        assert_eq!(parsed[0].props.get("p").unwrap(), b"v");
    }

    #[test]
    fn parse_iproplist_rejects_malformed_entries() {
        let iprops_item = SvnItem::List(vec![SvnItem::List(vec![SvnItem::String(
            b"/trunk".to_vec(),
        )])]);
        let err = parse_iproplist(&iprops_item).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "iproplist entry too short"));

        let iprops_item = SvnItem::List(vec![SvnItem::Number(1)]);
        let err = parse_iproplist(&iprops_item).unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "iproplist entry not a list"));
    }

    #[test]
    fn parse_get_file_response_params_supports_checksum_and_iprops() {
        let checksum_tuple = SvnItem::List(vec![SvnItem::String(b"sha1:abc".to_vec())]);
        let props_item = SvnItem::List(vec![SvnItem::List(vec![
            SvnItem::String(b"p".to_vec()),
            SvnItem::String(b"v".to_vec()),
        ])]);
        let iprops_item = SvnItem::List(vec![SvnItem::List(vec![
            SvnItem::String(b"/".to_vec()),
            SvnItem::List(Vec::new()),
        ])]);
        let iprops_tuple = SvnItem::List(vec![iprops_item]);

        let params = vec![
            checksum_tuple,
            SvnItem::Number(42),
            props_item,
            iprops_tuple,
        ];
        let parsed = parse_get_file_response_params(&params).unwrap();
        assert_eq!(parsed.rev, 42);
        assert_eq!(parsed.checksum.as_deref(), Some("sha1:abc"));
        assert_eq!(parsed.props.get("p").unwrap(), b"v");
        assert_eq!(parsed.inherited_props.len(), 1);
        assert_eq!(parsed.inherited_props[0].path, "/");
    }

    #[test]
    fn parse_repos_info_reads_uuid_root_and_caps() {
        let params = vec![
            SvnItem::String(b"uuid".to_vec()),
            SvnItem::String(b"svn://example.com/repo".to_vec()),
            SvnItem::List(vec![
                SvnItem::Word("mergeinfo".to_string()),
                SvnItem::Word("log-revprops".to_string()),
            ]),
        ];

        let info = parse_repos_info(&params).unwrap();
        assert_eq!(info.uuid, "uuid");
        assert_eq!(info.root_url, "svn://example.com/repo");
        assert_eq!(info.capabilities.len(), 2);
    }

    #[test]
    fn parse_repos_info_rejects_malformed_caps() {
        let params = vec![
            SvnItem::String(b"uuid".to_vec()),
            SvnItem::String(b"svn://example.com/repo".to_vec()),
            SvnItem::List(vec![SvnItem::Number(1)]),
        ];

        let err = parse_repos_info(&params).unwrap_err();
        assert!(
            matches!(err, SvnError::Protocol(msg) if msg == "repos-info caps entry not a word")
        );
    }

    #[test]
    fn parse_repos_info_accepts_missing_root_and_caps() {
        let params = vec![SvnItem::String(b"uuid".to_vec())];

        let info = parse_repos_info(&params).unwrap();
        assert_eq!(info.uuid, "uuid");
        assert!(info.root_url.is_empty());
        assert!(info.capabilities.is_empty());
    }

    #[test]
    fn parse_repos_info_accepts_missing_caps() {
        let params = vec![
            SvnItem::String(b"uuid".to_vec()),
            SvnItem::String(b"svn://example.com/repo".to_vec()),
        ];

        let info = parse_repos_info(&params).unwrap();
        assert_eq!(info.uuid, "uuid");
        assert_eq!(info.root_url, "svn://example.com/repo");
        assert!(info.capabilities.is_empty());
    }
}
