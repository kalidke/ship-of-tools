<script setup lang="ts">
// A muted, looping demo clip in a frame: <DemoLoop name="hero" caption="..."/>
// plays assets/media/<name>.webm (or .mp4) with <name>.png as the poster.
// The poster alone is shown on narrow screens, under prefers-reduced-motion,
// and before hydration; a click pauses or resumes the clip, and the Enlarge
// button puts it into fullscreen. Below 768 px the poster is <name>-phone.png
// when that file exists (a crop that stays readable on a phone).
// The figure carries the clip's aspect ratio as --sot-aspect, so CSS can cap
// the frame's width to fit a height budget without letterboxing, and its
// display width as --sot-w (see frameStyle in media.ts).
import { computed, onMounted, ref } from 'vue'
import { frameStyle, mediaSize, mediaUrl } from './media'

const props = defineProps<{ name: string; caption?: string }>()
const webm = computed(() => mediaUrl(`${props.name}.webm`))
const mp4 = computed(() => mediaUrl(`${props.name}.mp4`))
const poster = computed(() => mediaUrl(`${props.name}.png`))
const size = computed(() => mediaSize(`${props.name}.png`))
const phone = computed(() => mediaUrl(`${props.name}-phone.png`))
const phoneSize = computed(() => mediaSize(`${props.name}-phone.png`))
const play = ref(false)
const video = ref<HTMLVideoElement | null>(null)

onMounted(() => {
  const narrow = window.matchMedia('(max-width: 767px)').matches
  const still = window.matchMedia('(prefers-reduced-motion: reduce)').matches
  play.value = !narrow && !still && !!(webm.value || mp4.value)
})

function toggle() {
  const v = video.value
  if (!v) return
  v.paused ? v.play() : v.pause()
}

function enlarge() {
  video.value?.requestFullscreen?.()
}
</script>

<template>
  <figure class="sot-media" :style="frameStyle(size)">
    <div class="sot-window">
      <template v-if="play">
        <video
          ref="video"
          autoplay
          muted
          loop
          playsinline
          preload="metadata"
          :poster="poster"
          :width="size?.[0]"
          :height="size?.[1]"
          :aria-label="caption || name"
          title="Click to pause or play"
          @click="toggle"
        >
          <source v-if="webm" :src="webm" type="video/webm" />
          <source v-if="mp4" :src="mp4" type="video/mp4" />
        </video>
        <button
          type="button"
          class="sot-enlarge"
          :aria-label="`Enlarge: ${caption || name}`"
          title="Show full screen"
          @click="enlarge"
        >Enlarge</button>
      </template>
      <picture v-else-if="poster">
        <source
          v-if="phone"
          media="(max-width: 767px)"
          :srcset="phone"
          :width="phoneSize?.[0]"
          :height="phoneSize?.[1]"
        />
        <img :src="poster" :alt="caption || name" :width="size?.[0]" :height="size?.[1]" />
      </picture>
      <div v-else class="sot-media-empty" :aria-label="caption || name" />
    </div>
    <figcaption v-if="caption">{{ caption }}</figcaption>
  </figure>
</template>
