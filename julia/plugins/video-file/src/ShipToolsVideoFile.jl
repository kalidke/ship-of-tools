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
# Poster extraction shells out to `ffmpeg` on the host where the file lives
# (often a remote backend), consistent with the backend-side-decode model.

using ConceptExplorerCore

export VideoFile

"""
    VideoFile <: FileType

Built-in plugin for video files. `preview` returns a PNG poster frame; the
browser handles playback (see the frontend `o` → `video.open` path).
"""
struct VideoFile <: ConceptExplorerCore.FileType end

const VIDEO_EXTENSIONS = (".mp4", ".webm", ".mov", ".mkv", ".m4v")

ConceptExplorerCore.matches(::Type{VideoFile}, path::AbstractString) =
    any(endswith(lowercase(path), ext) for ext in VIDEO_EXTENSIONS)

"""
    preview(::Type{VideoFile}, path) -> PreviewPayload

Returns the first frame as a lossless `image/png` poster (full resolution; the
frontend downsamples if it exceeds the GPU texture cap). When ffmpeg is
unavailable or extraction fails, returns a `text/markdown` note explaining how
to view the video — never a silent blank pane.
"""
function ConceptExplorerCore.preview(::Type{VideoFile}, path::AbstractString)
    ffmpeg = Sys.which("ffmpeg")
    if ffmpeg === nothing
        return _note("`ffmpeg` not found on the kernel host — install it to show a video poster. Playback opens in the browser with `o`.")
    end
    poster = try
        read(`$ffmpeg -loglevel error -ss 0 -i $path -frames:v 1 -f image2pipe -vcodec png -`)
    catch e
        return _note("Couldn't extract a poster frame: $(sprint(showerror, e))\n\nPlayback still opens in the browser with `o`.")
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
