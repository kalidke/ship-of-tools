module ShipToolsVideoFile

# Built-in plugin for video files (`.mp4`, `.webm`, `.mov`, `.mkv`, `.m4v`).
#
# The preview pane shows a single still **poster frame** — actual playback
# happens in the OS browser via the `o` key (HTML5 <video>, native hardware
# decode), which is both far higher quality and smoother than streaming
# decoded frames over the wire. So this plugin's only job is to extract one
# representative frame, full-resolution and lossless (PNG), which the
# frontend's existing image preview path renders directly.
#
# Poster extraction runs `ffmpeg` on the host where the file lives (often a
# remote backend), consistent with the backend-side-decode model. It is
# `FFMPEG_jll`'s own build, run through its command form; nothing is looked up
# on the search path.

using ConceptExplorerCore
using FFMPEG_jll

export VideoFile

"""
    VideoFile <: FileType

Built-in plugin for video files. `preview` returns a PNG poster frame; the
browser handles playback (see the frontend `o` → `video.open` path).
"""
struct VideoFile <: ConceptExplorerCore.FileType end

const VIDEO_EXTENSIONS = (".mp4", ".webm", ".mov", ".mkv", ".m4v")

# The tool as the JLL's command form (a `Cmd` with its library path set). A function, so a platform with no ready-built
# ffmpeg still loads this module; a test stands another command in here.
const FFMPEG = Ref{Function}(() -> FFMPEG_jll.ffmpeg())

ConceptExplorerCore.matches(::Type{VideoFile}, path::AbstractString) =
    any(endswith(lowercase(path), ext) for ext in VIDEO_EXTENSIONS)

"""
    preview(::Type{VideoFile}, path) -> PreviewPayload

Returns the first frame as a lossless `image/png` poster (full resolution; the
frontend downsamples if it exceeds the GPU texture cap). When extraction fails
or outlasts `ConceptExplorerCore.TOOL_WAIT`, returns a `text/markdown` note explaining how to view
the video — never a silent blank pane.
"""
ConceptExplorerCore.preview(::Type{VideoFile}, path::AbstractString) = _poster(path, ConceptExplorerCore.TOOL_WAIT)

function _poster(path::AbstractString, bound::Real)
    if !FFMPEG_jll.is_available()
        return _note("no ready-built ffmpeg exists for this platform. Playback opens in the browser with `o`.")
    end
    poster = try
        ConceptExplorerCore.read_within(`$(FFMPEG[]()) -loglevel error -ss 0 -i $path -frames:v 1 -f image2pipe -vcodec png -`, bound)
    catch e
        return _note("Couldn't extract a poster frame: $(ConceptExplorerCore.error_words(e))\n\nPlayback still opens in the browser with `o`.")
    end
    isempty(poster) && return _note("Poster extraction produced no frame. Playback opens in the browser with `o`.")
    ConceptExplorerCore.PreviewPayload("image/png", poster)
end

_note(msg) = ConceptExplorerCore.PreviewPayload(
    "text/markdown",
    Vector{UInt8}(codeunits("# video\n\n$msg\n")),
)

"""
    parse_entities(::Type{VideoFile}, path) -> Vector{ConceptEntity}

Phase-1 stub — videos contribute no concept entities.
"""
ConceptExplorerCore.parse_entities(::Type{VideoFile}, path::AbstractString) =
    ConceptExplorerCore.ConceptEntity[]

end # module
