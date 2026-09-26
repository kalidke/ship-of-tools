<script setup lang="ts">
// A still screenshot in a frame: <DemoShot name="preview-math" caption="..."/>
// shows assets/media/<name>.png; <DemoShot path="readme/sessions-crop.png" .../>
// shows any image under assets/. The image links to the full-size file.
import { computed } from 'vue'
import { assetSize, assetUrl, frameStyle, mediaSize, mediaUrl } from './media'

const props = defineProps<{ name?: string; path?: string; caption?: string }>()
const src = computed(() => (props.path ? assetUrl(props.path) : mediaUrl(`${props.name}.png`)))
const size = computed(() => (props.path ? assetSize(props.path) : mediaSize(`${props.name}.png`)))
const label = computed(() => props.caption || props.name || props.path || '')
</script>

<template>
  <figure class="sot-media" :style="frameStyle(size)">
    <div class="sot-window">
      <a v-if="src" :href="src" target="_blank" rel="noopener" title="Open full size">
        <img :src="src" :alt="label" :width="size?.[0]" :height="size?.[1]" loading="lazy" />
      </a>
      <div v-else class="sot-media-empty" :aria-label="label" />
    </div>
    <figcaption v-if="caption">{{ caption }}</figcaption>
  </figure>
</template>
