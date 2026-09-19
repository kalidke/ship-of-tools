# A minimal RFC 8259 JSON reader/writer for ShipToolsRepl's wire protocol.
#
# Why this exists instead of `using JSON3`: ShipToolsRepl is STACKED under the
# user's project via `JULIA_LOAD_PATH` (see `spawn_supervisor_with_project` in
# rust/backend/src/repl.rs and ADR 0032 §2), so any registered dependency of
# this package can be resolved from the USER's manifest instead of ours. A
# CairoMakie-pinned Parsers 3.0.0 shadowed JSON3's own Parsers dependency (no
# JSON3 release supports Parsers 3), and the shim failed to precompile for
# every such user (2026-09-18 field failure). The invariant going forward:
# ShipToolsRepl's `[deps]` are stdlib only, so nothing here can ever be
# shadowed — see the guard test in test/runtests.jl. This file is what that
# invariant costs: a codec covering exactly what the wire protocol needs, no
# more.
#
# No regex, no `JSON3.Object` sugar — a plain recursive-descent parser over
# the request's bytes, and a writer keyed on Julia types rather than an
# intermediate tree. Objects parse to `Dict{Symbol,Any}` so the existing
# `get(req, :id, ...)` idiom elsewhere in this file keeps working unchanged.

# ---- reading ---------------------------------------------------------------

"""
    json_read(s::AbstractString) -> Any

Parse one JSON value out of `s`. Objects become `Dict{Symbol,Any}`, arrays
become `Vector{Any}`, numbers become `Int64` when they parse as an integral
token in range (else `Float64`), strings become `String` (with `\\uXXXX`
escapes, including surrogate pairs, decoded to the actual character), and
`true`/`false`/`null` become `true`/`false`/`nothing`. Throws `ArgumentError`
on anything malformed — `serve`'s per-line `try`/`catch` turns that into a
`repl.parse_error` response, same as a `JSON3.read` throw did before.
"""
function json_read(s::AbstractString)
    bytes = Vector{UInt8}(String(s))
    len = length(bytes)
    value, pos = read_value(bytes, 1, len)
    pos = skip_ws(bytes, pos, len)
    pos <= len && throw(ArgumentError("trailing data after JSON value at byte $pos"))
    return value
end

function skip_ws(bytes::Vector{UInt8}, pos::Int, len::Int)
    while pos <= len
        b = bytes[pos]
        (b == UInt8(' ') || b == UInt8('\t') || b == UInt8('\n') || b == UInt8('\r')) || break
        pos += 1
    end
    return pos
end

function read_value(bytes::Vector{UInt8}, pos::Int, len::Int)
    pos = skip_ws(bytes, pos, len)
    pos <= len || throw(ArgumentError("unexpected end of input"))
    b = bytes[pos]
    if b == UInt8('{')
        return read_object(bytes, pos, len)
    elseif b == UInt8('[')
        return read_array(bytes, pos, len)
    elseif b == UInt8('"')
        return read_string(bytes, pos, len)
    elseif b == UInt8('t')
        return read_literal(bytes, pos, len, "true", true)
    elseif b == UInt8('f')
        return read_literal(bytes, pos, len, "false", false)
    elseif b == UInt8('n')
        return read_literal(bytes, pos, len, "null", nothing)
    elseif b == UInt8('-') || (UInt8('0') <= b <= UInt8('9'))
        return read_number(bytes, pos, len)
    else
        throw(ArgumentError("unexpected byte '$(Char(b))' at $pos"))
    end
end

function read_literal(bytes::Vector{UInt8}, pos::Int, len::Int, lit::String, val)
    n = ncodeunits(lit)
    pos + n - 1 <= len || throw(ArgumentError("truncated literal at byte $pos"))
    for i in 1:n
        bytes[pos + i - 1] == UInt8(lit[i]) || throw(ArgumentError("invalid literal at byte $pos"))
    end
    return val, pos + n
end

isdigit_byte(b::UInt8) = UInt8('0') <= b <= UInt8('9')

function read_number(bytes::Vector{UInt8}, pos::Int, len::Int)
    start = pos
    isint = true
    bytes[pos] == UInt8('-') && (pos += 1)
    d0 = pos
    while pos <= len && isdigit_byte(bytes[pos])
        pos += 1
    end
    pos == d0 && throw(ArgumentError("invalid number at byte $start"))
    if pos <= len && bytes[pos] == UInt8('.')
        isint = false
        pos += 1
        f0 = pos
        while pos <= len && isdigit_byte(bytes[pos])
            pos += 1
        end
        pos == f0 && throw(ArgumentError("invalid number at byte $start"))
    end
    if pos <= len && (bytes[pos] == UInt8('e') || bytes[pos] == UInt8('E'))
        isint = false
        pos += 1
        (pos <= len && (bytes[pos] == UInt8('+') || bytes[pos] == UInt8('-'))) && (pos += 1)
        e0 = pos
        while pos <= len && isdigit_byte(bytes[pos])
            pos += 1
        end
        pos == e0 && throw(ArgumentError("invalid number at byte $start"))
    end
    tok = String(bytes[start:pos - 1])
    if isint
        iv = tryparse(Int64, tok)
        return (iv === nothing ? parse(Float64, tok) : iv), pos
    end
    return parse(Float64, tok), pos
end

function hexval(b::UInt8)
    UInt8('0') <= b <= UInt8('9') && return UInt16(b - UInt8('0'))
    UInt8('a') <= b <= UInt8('f') && return UInt16(b - UInt8('a') + 10)
    UInt8('A') <= b <= UInt8('F') && return UInt16(b - UInt8('A') + 10)
    throw(ArgumentError("invalid \\u hex digit '$(Char(b))'"))
end

function read_hex4(bytes::Vector{UInt8}, pos::Int, len::Int)
    pos + 3 <= len || throw(ArgumentError("truncated \\u escape at byte $pos"))
    v = UInt16(0)
    for i in 0:3
        v = (v << 4) | hexval(bytes[pos + i])
    end
    return v, pos + 4
end

# Manual UTF-8 encoding (rather than `Char(cp)`) so a combined surrogate-pair
# codepoint writes out the same way a plain BMP one does, with one code path.
function write_utf8!(buf::IO, cp::UInt32)
    if cp <= 0x7f
        write(buf, UInt8(cp))
    elseif cp <= 0x7ff
        write(buf, UInt8(0xc0 | (cp >> 6)))
        write(buf, UInt8(0x80 | (cp & 0x3f)))
    elseif cp <= 0xffff
        write(buf, UInt8(0xe0 | (cp >> 12)))
        write(buf, UInt8(0x80 | ((cp >> 6) & 0x3f)))
        write(buf, UInt8(0x80 | (cp & 0x3f)))
    else
        write(buf, UInt8(0xf0 | (cp >> 18)))
        write(buf, UInt8(0x80 | ((cp >> 12) & 0x3f)))
        write(buf, UInt8(0x80 | ((cp >> 6) & 0x3f)))
        write(buf, UInt8(0x80 | (cp & 0x3f)))
    end
    return nothing
end

function read_string(bytes::Vector{UInt8}, pos::Int, len::Int)
    bytes[pos] == UInt8('"') || throw(ArgumentError("expected string at byte $pos"))
    pos += 1
    buf = IOBuffer()
    while true
        pos <= len || throw(ArgumentError("unterminated string"))
        b = bytes[pos]
        if b == UInt8('"')
            return String(take!(buf)), pos + 1
        elseif b == UInt8('\\')
            pos += 1
            pos <= len || throw(ArgumentError("unterminated escape"))
            e = bytes[pos]
            if e == UInt8('"'); write(buf, '"'); pos += 1
            elseif e == UInt8('\\'); write(buf, '\\'); pos += 1
            elseif e == UInt8('/'); write(buf, '/'); pos += 1
            elseif e == UInt8('b'); write(buf, '\b'); pos += 1
            elseif e == UInt8('f'); write(buf, '\f'); pos += 1
            elseif e == UInt8('n'); write(buf, '\n'); pos += 1
            elseif e == UInt8('r'); write(buf, '\r'); pos += 1
            elseif e == UInt8('t'); write(buf, '\t'); pos += 1
            elseif e == UInt8('u')
                pos += 1
                cu, pos = read_hex4(bytes, pos, len)
                if 0xD800 <= cu <= 0xDBFF
                    # High surrogate: must be immediately followed by a low
                    # surrogate `\u` escape (RFC 8259 doesn't allow a bare
                    # supplementary-plane codepoint any other way).
                    (pos + 1 <= len && bytes[pos] == UInt8('\\') && bytes[pos + 1] == UInt8('u')) ||
                        throw(ArgumentError("unpaired high surrogate at byte $pos"))
                    pos += 2
                    cu2, pos = read_hex4(bytes, pos, len)
                    (0xDC00 <= cu2 <= 0xDFFF) ||
                        throw(ArgumentError("invalid low surrogate at byte $pos"))
                    cp = 0x10000 + ((UInt32(cu) - 0xD800) << 10) + (UInt32(cu2) - 0xDC00)
                    write_utf8!(buf, cp)
                elseif 0xDC00 <= cu <= 0xDFFF
                    throw(ArgumentError("unpaired low surrogate at byte $pos"))
                else
                    write_utf8!(buf, UInt32(cu))
                end
            else
                throw(ArgumentError("invalid escape '\\$(Char(e))' at byte $pos"))
            end
        else
            # Raw byte pass-through — this is how multibyte UTF-8 sequences in
            # the input (an already-valid string, unescaped) survive intact.
            write(buf, b)
            pos += 1
        end
    end
end

function read_object(bytes::Vector{UInt8}, pos::Int, len::Int)
    bytes[pos] == UInt8('{') || throw(ArgumentError("expected object at byte $pos"))
    pos = skip_ws(bytes, pos + 1, len)
    d = Dict{Symbol,Any}()
    (pos <= len && bytes[pos] == UInt8('}')) && return d, pos + 1
    while true
        pos = skip_ws(bytes, pos, len)
        key, pos = read_string(bytes, pos, len)
        pos = skip_ws(bytes, pos, len)
        (pos <= len && bytes[pos] == UInt8(':')) || throw(ArgumentError("expected ':' at byte $pos"))
        val, pos = read_value(bytes, pos + 1, len)
        d[Symbol(key)] = val
        pos = skip_ws(bytes, pos, len)
        pos <= len || throw(ArgumentError("unterminated object"))
        if bytes[pos] == UInt8(',')
            pos += 1
        elseif bytes[pos] == UInt8('}')
            return d, pos + 1
        else
            throw(ArgumentError("expected ',' or '}' at byte $pos"))
        end
    end
end

function read_array(bytes::Vector{UInt8}, pos::Int, len::Int)
    bytes[pos] == UInt8('[') || throw(ArgumentError("expected array at byte $pos"))
    pos = skip_ws(bytes, pos + 1, len)
    v = Any[]
    (pos <= len && bytes[pos] == UInt8(']')) && return v, pos + 1
    while true
        val, pos = read_value(bytes, pos, len)
        push!(v, val)
        pos = skip_ws(bytes, pos, len)
        pos <= len || throw(ArgumentError("unterminated array"))
        if bytes[pos] == UInt8(',')
            pos += 1
        elseif bytes[pos] == UInt8(']')
            return v, pos + 1
        else
            throw(ArgumentError("expected ',' or ']' at byte $pos"))
        end
    end
end

# ---- writing ----------------------------------------------------------------

"""
    json_write(io::IO, x) -> Nothing

Write `x` as one JSON value to `io` (no trailing newline — `write_envelope`
appends its own). Dispatches on Julia type rather than building an
intermediate tree: `AbstractDict`/`NamedTuple` → object, `AbstractVector`/
`Tuple` → array, `AbstractString`/`Symbol`/`Char` → string, `Bool` → literal,
`Integer` → number, `AbstractFloat` → number (non-finite → `null`, because the
Rust side parses frames with `serde_json`, which has no NaN/Infinity token —
`rust/backend/src/repl.rs`'s `serde_json::from_str`/`Value` reject them),
`Nothing`/`Missing` → `null`. Anything else falls back to `string(x)` as a
JSON string, so an unanticipated value still produces valid JSON.
"""
json_write(io::IO, x::AbstractDict) = write_json_object(io, x)
json_write(io::IO, x::NamedTuple) = write_json_object(io, x)
json_write(io::IO, x::Union{AbstractVector,Tuple}) = write_json_array(io, x)
json_write(io::IO, x::AbstractString) = write_json_string(io, x)
json_write(io::IO, x::Symbol) = write_json_string(io, String(x))
json_write(io::IO, x::Char) = write_json_string(io, string(x))
json_write(io::IO, x::Bool) = print(io, x ? "true" : "false")
json_write(io::IO, x::Integer) = print(io, x)
json_write(io::IO, ::Nothing) = print(io, "null")
json_write(io::IO, ::Missing) = print(io, "null")
json_write(io::IO, x) = write_json_string(io, string(x))

function json_write(io::IO, x::AbstractFloat)
    isfinite(x) ? print(io, x) : print(io, "null")
end

function write_json_object(io::IO, x)
    print(io, '{')
    first = true
    for (k, v) in pairs(x)
        first || print(io, ',')
        first = false
        write_json_string(io, json_key_string(k))
        print(io, ':')
        json_write(io, v)
    end
    print(io, '}')
end

json_key_string(k::Symbol) = String(k)
json_key_string(k::AbstractString) = k
json_key_string(k) = string(k)

function write_json_array(io::IO, x)
    print(io, '[')
    first = true
    for v in x
        first || print(io, ',')
        first = false
        json_write(io, v)
    end
    print(io, ']')
end

# Escapes only what RFC 8259 requires (`"`, `\`, and control characters); any
# other Unicode character is written as its native UTF-8 bytes, which is legal
# JSON text and matches what a JSON parser on the other end (serde_json here)
# expects.
function write_json_string(io::IO, s::AbstractString)
    print(io, '"')
    for c in s
        if c == '"'
            print(io, "\\\"")
        elseif c == '\\'
            print(io, "\\\\")
        elseif c == '\n'
            print(io, "\\n")
        elseif c == '\r'
            print(io, "\\r")
        elseif c == '\t'
            print(io, "\\t")
        elseif Int(c) < 0x20
            print(io, "\\u", lpad(string(Int(c); base = 16), 4, '0'))
        else
            print(io, c)
        end
    end
    print(io, '"')
end
