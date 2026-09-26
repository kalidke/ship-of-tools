// Demo media resolved at BUILD time through Vite's glob import, so a missing
// file is simply absent from the map: the page still builds and the component
// renders an empty frame in its place. mediaUrl looks in assets/media/;
// assetUrl takes a path relative to assets/ (for crops and labelled stills).
// mediaSize and assetSize give a PNG's [width, height] (see config.mts).
import sizes from 'virtual:sot-media-sizes'

const files = import.meta.glob(
  ['../assets/media/*.{webm,mp4,png}', '../assets/readme/*.png', '../assets/*.png'],
  { query: '?url', import: 'default', eager: true },
) as Record<string, string>

export function mediaUrl(file: string): string | undefined {
  return files[`../assets/media/${file}`]
}

export function assetUrl(path: string): string | undefined {
  return files[`../assets/${path}`]
}

export function mediaSize(file: string): [number, number] | undefined {
  return sizes[`media/${file}`]
}

export function assetSize(path: string): [number, number] | undefined {
  return sizes[path]
}

// Every clip and still is shown at one text scale: a frame is at most
// MEDIA_SCALE times its file's pixel width, and never wider than the column,
// so a tight crop of a narrow window renders narrower instead of enlarged.
// At 0.75 a terminal cell (27 px at font scale 1.5) comes to 20 px; crops up
// to about 1380 px wide fit the widest doc column (1040 px) at that scale.
export const MEDIA_SCALE = 0.75

export function frameStyle(size: [number, number] | undefined): Record<string, string | number> | undefined {
  if (!size) return undefined
  return { '--sot-aspect': size[0] / size[1], '--sot-w': `${Math.round(size[0] * MEDIA_SCALE)}px` }
}
