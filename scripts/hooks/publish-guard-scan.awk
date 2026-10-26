# Prepare one selected frontier before its batch read, then consume that batch.
# Tree contexts use hex prefixes between rounds; matching always sees literal bytes.
function fail(message) {
    stopped=1
    print message > failure
    if (close(failure) != 0) exit 2
    exit 1
}
function emit(file,text) { print text >> file; written[file]=1 }
function cannot_read(id) { fail((phase == "trees" ? "cannot read tree " : "cannot read object ") id) }
function oid_ok(id) { return id != "" && id !~ /[^0-9a-f]/ }
function zero(id) { return id != "" && id !~ /[^0]/ }
function object_type(id) {
    if (!oid_ok(id)) fail("invalid reachable object id")
    if (type[id] !~ /^(commit|tag|tree|blob)$/) fail("object " id " is unavailable locally")
    return type[id]
}
function whole_word(text, entry,    start,p,after) {
    start=1
    while ((p=index(substr(text,start),entry)) > 0) {
        p+=start-1; after=p+length(entry)
        if ((p == 1 || substr(text,p-1,1) !~ /[A-Za-z0-9]/) &&
            (after > length(text) || substr(text,after,1) !~ /[A-Za-z0-9]/)) return 1
        start=p+1
    }
    return 0
}
function scan_line(text,label,    s,j,p) {
    s=tolower(text)
    for (j=1;j<=na;j++) while ((p=index(s,a[j])) > 0) s=substr(s,1,p-1) substr(s,p+length(a[j]))
    for (j=1;j<=nd;j++) if (whole_word(s,d[j])) fail(label " holds the list entry at line " ln[j])
}
function scan(text,label,    lines,n,j) {
    n=split(text,lines,"\n")
    for (j=1;j<=n;j++) scan_line(lines[j],label)
}
function load_policy(    r,s,n) {
    while ((r=(getline s < policy)) > 0) {
        n++; sub(/\r$/, "", s)
        if (s == "" || substr(s,1,1) == "#") continue
        if (substr(s,1,1) == "!") { if (length(s)>1) a[++na]=tolower(substr(s,2)); continue }
        d[++nd]=tolower(s); ln[nd]=n
    }
    if (r < 0 || close(policy) != 0) fail("scanner failed")
}
function load_catalog(    r,s,f) {
    while ((r=(getline s < catalog)) > 0) {
        if (split(s,f," ") != 2 || !oid_ok(f[1])) fail("cannot enumerate local objects")
        type[f[1]]=f[2]
    }
    if (r < 0 || close(catalog) != 0) fail("cannot look up local object")
}
function load_ids(file,ids,    r,s) {
    while ((r=(getline s < file)) > 0) {
        if (!oid_ok(s)) fail("scanner failed")
        ids[s]=1
    }
    if (r < 0 || close(file) != 0) fail("scanner failed")
}
function initial_refs(    r,s,f,ordinal,namespace) {
    while ((r=(getline s < updates)) > 0) {
        ordinal++
        if (split(s,f," ") != 4 || !oid_ok(f[2]) || !oid_ok(f[4])) fail("invalid push input")
        if (zero(f[2])) continue
        namespace=(f[3] ~ /^refs\/heads\// ? "branch" : f[3] ~ /^refs\/tags\// ? "tag" : "ref")
        scan_line(f[3],namespace " update " ordinal " (object " f[2] ")")
        emit(next_frontier,f[2])
    }
    if (r < 0 || close(updates) != 0) fail("cannot preserve push input")
}
function tree_context(text,    p) {
    p=index(text," ")
    context_id=(p ? substr(text,1,p-1) : text)
    context_hex=(p ? substr(text,p+1) : "")
    if (!oid_ok(context_id) || context_hex ~ /[^0-9a-f]/ || length(context_hex)%2) fail("scanner failed")
}
function prepare(    r,s,t) {
    if (phase == "old" || phase == "new") load_ids(excluded_file,excluded)
    if (phase == "new") load_ids(seen_file,seen)
    while ((r=(getline s < frontier)) > 0) {
        if (phase == "trees") {
            tree_context(s)
            s=context_id
            if (object_type(s) != "tree") fail("invalid tree object " s)
        } else if (phase == "old") {
            if (!oid_ok(s)) fail("invalid old object id")
            if (excluded[s] || type[s] !~ /^(commit|tag)$/) continue
            excluded[s]=1; emit(excluded_file,s)
        } else if (phase == "new") {
            if (!oid_ok(s)) fail("invalid reachable object id")
            if (seen[s]) continue
            t=object_type(s)
            seen[s]=1; emit(seen_file,s)
            if (t == "commit" && excluded[s]) continue
            if (t == "tree") { emit(roots,s); continue }
            if (t == "blob") { emit(blobs,s); continue }
        } else {
            if (object_type(s) != "blob") fail("cannot read object " s)
        }
        if (!batch_seen[s]++) emit(requests,s)
    }
    if (r < 0 || close(frontier) != 0) fail("scanner failed")
}
function load_contexts(    r,s) {
    while ((r=(getline s < frontier)) > 0) {
        tree_context(s)
        context[context_id,++contexts[context_id]]=context_hex
    }
    if (r < 0 || close(frontier) != 0) fail("scanner failed")
}
function unhex(text,    s,j,b) {
    s=""
    for (j=1;j<=length(text);j+=2) {
        b=digit[substr(text,j,1)]*16+digit[substr(text,j+1,1)]
        s=s ch[b]
    }
    return s
}
function header(text,    f) {
    if ((getline expected < requests) != 1) fail("scanner failed")
    if (split(text,f," ") != 3 || f[1] != expected || f[2] != type[expected] || f[3] !~ /^[0-9]+$/)
        cannot_read(expected)
    id=expected; kind=f[2]; remaining=f[3]+0
    if ((phase == "trees" && kind != "tree") || (phase == "blobs" && kind != "blob") ||
            ((phase == "old" || phase == "new") && kind !~ /^(commit|tag)$/)) cannot_read(id)
    label=(kind == "tag" ? "tag object" : kind) " " id
    state=(remaining ? "body" : "separator")
    tree_state="mode"; tree_mode=""; tree_mode_value=0; tree_name=""; tree_hex=""; tree_child=""; tree_bytes=0
    graph_line=""; graph_prefix=""; graph_bytes=0; graph_headers=1; first_line=1
    tag_target=""; has_target=0; match_line=""
}
function graph_header(    line,prefix,bytes) {
    line=graph_line; prefix=graph_prefix; bytes=graph_bytes
    graph_line=""; graph_prefix=""; graph_bytes=0
    if (kind == "tag") {
        if (first_line && prefix == "object ") { tag_target=substr(line,8); has_target=1 }
        first_line=0
    } else if (graph_headers) {
        if (bytes == 0) graph_headers=0
        else if (prefix == "parent ") emit(next_frontier,substr(line,8))
        else if (phase == "new" && substr(prefix,1,5) == "tree ") emit(next_frontier,substr(line,6))
    }
}
function canonical_mode(bits,    file_type) {
    # Git tree-walk.c decodes modes through object.h:canon_mode().
    file_type=int(bits/4096)%16
    if (file_type == 8) return (int(bits/64)%2 ? "100755" : "100644")
    if (file_type == 10) return "120000"
    if (file_type == 4) return "40000"
    return "160000"
}
function tree_byte(b,    k) {
    if (tree_state == "mode") {
        if (b == 32) {
            if (tree_mode == "") invalid_tree[id]=1
            tree_state="name"
        } else {
            if (b < 48 || b > 55) invalid_tree[id]=1
            tree_mode=tree_mode ch[b]; tree_mode_value=(tree_mode_value*8+b-48)%65536
        }
    } else if (tree_state == "name") {
        if (b == 0) {
            if (tree_name == "") invalid_tree[id]=1
            tree_state="oid"
        }
        else { tree_name=tree_name ch[b]; tree_hex=tree_hex hex[b] }
    } else {
        tree_child=tree_child hex[b]
        if (++tree_bytes == length(id)/2) {
            k=++entries[id]
            mode[id,k]=canonical_mode(tree_mode_value); name[id,k]=tree_name
            name_hex[id,k]=tree_hex; child[id,k]=tree_child
            tree_state="mode"; tree_mode=""; tree_mode_value=0; tree_name=""; tree_hex=""; tree_child=""; tree_bytes=0
        }
    }
}
function scan_tree_level(tree,    c,k,prefix,path,entry_mode,entry_child) {
    if (invalid_tree[tree]) fail("cannot read tree " tree)
    for (c=1;c<=contexts[tree];c++) {
        prefix=context[tree,c]
        path=unhex(prefix)
        for (k=1;k<=entries[tree];k++) {
            entry_mode=mode[tree,k]; entry_child=child[tree,k]
            scan(path name[tree,k],"tree " tree)
            if (entry_mode == "160000") scan_line(entry_child,"tree " tree)
            else if (entry_mode == "40000") emit(next_frontier,entry_child " " prefix name_hex[tree,k] "2f")
            else if (object_type(entry_child) == "blob") emit(blobs,entry_child)
            # As in the original object queue, a file-mode entry is dispatched
            # by its actual object type, including nonblob targets in raw trees.
            else emit(late,entry_child)
        }
    }
}
function finish_object() {
    if (phase == "trees") {
        if (tree_state != "mode" || tree_mode != "") invalid_tree[id]=1
        scan_tree_level(id)
    } else {
        if (graph_bytes) graph_header()
        if (phase == "new" && match_line != "") scan_line(match_line,label)
        if (kind == "tag") {
            if (phase == "new" && !oid_ok(tag_target)) fail("invalid tag object " id)
            if (has_target) emit(next_frontier,tag_target)
        }
    }
}
function metadata_byte(b) {
    if (state == "header") {
        if (b == 10) { header(batch_header); batch_header="" }
        else batch_header=batch_header ch[b]
    } else if (state == "separator") {
        if (b != 10) cannot_read(id)
        if (action != "failed") finish_object()
        state="header"
    } else {
        if (action == "failed") {
            if (--remaining == 0) state="separator"
            return
        }
        if (kind == "tree") tree_byte(b)
        else {
            if (phase == "new") {
                if (b == 0 || b == 10) { scan_line(match_line,label); match_line="" }
                else match_line=match_line ch[b]
            }
            if ((kind == "commit" && graph_headers) || (kind == "tag" && first_line)) {
                # Header prefixes and blank lines use raw bytes; queued ids
                # discard NUL as Bash read did in the original traversal.
                if (b == 10) graph_header()
                else {
                    if (++graph_bytes <= 7) graph_prefix=graph_prefix (b == 0 ? "\001" : ch[b])
                    if (b != 0) graph_line=graph_line ch[b]
                }
            }
        }
        if (--remaining == 0) state="separator"
    }
}
BEGIN {
    policy=dir "/list"; catalog=dir "/objects"; failure=dir "/failure"
    updates=dir "/updates"; excluded_file=dir "/excluded"; seen_file=dir "/seen"; blobs=dir "/blobs"
    requests=dir "/requests"
    if (action == "refs" || (action == "read" && phase != "old")) load_policy()
    if (action != "refs") load_catalog()
    if (action == "refs") { initial_refs(); prepared=1; exit }
    if (action == "prepare") { prepare(); prepared=1; exit }
    state="header"
    if (phase != "blobs" || action == "failed") {
        for (i=1;i<=255;i++) { ch[i]=sprintf("%c",i); hex[i]=sprintf("%02x",i) }
        hex[0]="00"
        for (i=0;i<16;i++) digit[substr("0123456789abcdef",i+1,1)]=i
    }
    if (phase == "trees" && action == "read") load_contexts()
}
(action == "read" && phase != "blobs") || action == "failed" {
    for (byte_index=1;byte_index<=NF;byte_index++) {
        # The exclusion walk needs raw headers only; its messages remain unread
        # by the matcher, while framing still accounts for every batch byte.
        if (state == "body" && (action == "failed" || (phase == "old" &&
                ((kind == "commit" && !graph_headers) || (kind == "tag" && !first_line))))) {
            skipped=NF-byte_index+1
            if (skipped > remaining) skipped=remaining
            remaining-=skipped; byte_index+=skipped-1
            if (remaining == 0) state="separator"
            continue
        }
        if ($byte_index !~ /^[0-9]+$/ || $byte_index > 255) fail("scanner failed")
        metadata_byte($byte_index+0)
    }
    next
}
action == "read" && phase == "blobs" {
    if (state == "header") header($0)
    else if (state == "separator") {
        if ($0 != "") fail("cannot read object " id)
        state="header"
    } else {
        n=length($0)
        if (remaining < n) fail("cannot read object " id)
        scan_line($0,label)
        if (remaining == n) { remaining=0; state="header" }
        else { remaining-=n+1; if (remaining == 0) state="separator" }
    }
    next
}
END {
    if (stopped) exit 1
    if (action == "failed") {
        # A failed batch may stop in a body or before its next header. Inspect
        # only the bytes already produced; never re-read or match after failure.
        if (state == "header" && (getline expected < requests) == 1) id=expected
        if (!oid_ok(id)) fail("scanner failed")
        cannot_read(id)
    }
    if (!prepared) {
        if (state != "header" || batch_header != "") fail("scanner failed")
        if ((getline expected < requests) != 0 || close(requests) != 0) fail("scanner failed")
    }
    for (file in written) if (close(file) != 0) fail("scanner failed")
    print "ok" > (dir "/scanner-ok")
    if (close(dir "/scanner-ok") != 0) fail("scanner failed")
}
