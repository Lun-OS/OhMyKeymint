<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import {
  MiuixButton,
  MiuixCard,
  MiuixDialog,
  MiuixProgressIndicator,
  MiuixSwitchPreference,
} from 'miuix-vue'
import {
  Cli,
  DEFAULT_KEYBOX_REMOTE_PROXY,
  DEFAULT_KEYBOX_REMOTE_URL,
  MAX_KEYBOX_REMOTE_INTERVAL_MINUTES,
  MIN_KEYBOX_REMOTE_INTERVAL_MINUTES,
  type KeyboxRemoteSettings,
} from '../cli'
import { i18n } from '../i18n'
import { isDev } from '../utils/dev'

type LoadStatus = 'loading' | 'ready' | 'error'

const props = defineProps<{ modelValue: boolean, cli: Cli }>()
const emit = defineEmits<{
  'update:modelValue': [value: boolean]
  notify: [message: string, error?: boolean]
  changed: []
}>()

const status = ref<LoadStatus>('loading')
const errorMessage = ref('')
const enabled = ref(false)
const url = ref('')
const proxy = ref('')
// v-model on a number input yields a number, so both parts stay loose.
const intervalHours = ref<string | number>('6')
const intervalMinutes = ref<string | number>('0')
const saved = ref<KeyboxRemoteSettings>({
  enabled: false,
  url: '',
  proxy: DEFAULT_KEYBOX_REMOTE_PROXY,
  interval_minutes: 360,
})
const busy = ref(false)
const fetching = ref(false)
let generation = 0

const preview = isDev()

function tr(key: string, fallback: string, ...args: unknown[]): string {
  const value = i18n.t(key, ...args)
  if (value !== key) return value
  let index = 0
  return fallback.replace(/%s/g, () => String(args[index++] ?? ''))
}

function isHttpsOrEmpty(value: string): boolean {
  return value === '' || /^https:\/\/[^\s]+$/i.test(value)
}

function parseIntervalPart(value: string | number): number | null {
  const text = String(value).trim()
  if (!/^\d{1,4}$/.test(text)) return null
  return Number.parseInt(text, 10)
}

const intervalTotalMinutes = computed<number | null>(() => {
  const hours = parseIntervalPart(intervalHours.value)
  const minutes = parseIntervalPart(intervalMinutes.value)
  if (hours === null || minutes === null || minutes > 59) return null
  return hours * 60 + minutes
})

const intervalValid = computed(() => intervalTotalMinutes.value !== null
  && intervalTotalMinutes.value >= MIN_KEYBOX_REMOTE_INTERVAL_MINUTES
  && intervalTotalMinutes.value <= MAX_KEYBOX_REMOTE_INTERVAL_MINUTES)

function splitInterval(totalMinutes: number): void {
  intervalHours.value = String(Math.floor(totalMinutes / 60))
  intervalMinutes.value = String(totalMinutes % 60)
}

const dirty = computed(() => enabled.value !== saved.value.enabled
  || url.value.trim() !== saved.value.url
  || proxy.value.trim() !== saved.value.proxy
  || intervalTotalMinutes.value !== saved.value.interval_minutes)

const valid = computed(() => isHttpsOrEmpty(url.value.trim())
  && isHttpsOrEmpty(proxy.value.trim())
  && intervalValid.value)

const canApply = computed(() => !preview && !busy.value && status.value === 'ready'
  && dirty.value && valid.value)

const canFetch = computed(() => !preview && !busy.value && !fetching.value
  && status.value === 'ready' && valid.value)

const anyBusy = computed(() => busy.value || fetching.value)

async function load(): Promise<void> {
  if (!props.modelValue) return
  const currentGeneration = ++generation
  status.value = 'loading'
  errorMessage.value = ''
  try {
    const state = preview
      ? {
          enabled: false,
          url: '',
          proxy: DEFAULT_KEYBOX_REMOTE_PROXY,
          interval_minutes: 360,
        }
      : await props.cli.getKeyboxRemoteSettings()
    if (currentGeneration !== generation || !props.modelValue) return
    enabled.value = state.enabled
    url.value = state.url
    proxy.value = state.proxy
    splitInterval(state.interval_minutes)
    saved.value = state
    status.value = 'ready'
  } catch (error) {
    if (currentGeneration !== generation || !props.modelValue) return
    status.value = 'error'
    errorMessage.value = error instanceof Error ? error.message : String(error)
  }
}

watch(() => props.modelValue, open => {
  if (open) void load()
  else generation++
})

function requestClose(): boolean {
  if (anyBusy.value) return false
  emit('update:modelValue', false)
  return true
}

defineExpose({ requestClose, busy: anyBusy })

async function save(): Promise<void> {
  await props.cli.setKeyboxRemoteSettings(
    enabled.value,
    url.value.trim(),
    proxy.value.trim(),
    intervalTotalMinutes.value ?? 0,
  )
  saved.value = {
    enabled: enabled.value,
    url: url.value.trim(),
    proxy: proxy.value.trim(),
    interval_minutes: intervalTotalMinutes.value ?? 0,
  }
}

async function apply(): Promise<void> {
  if (!canApply.value) return
  busy.value = true
  errorMessage.value = ''
  try {
    if (!preview) await save()
    emit('notify', tr('keybox_auto_fetch_saved', 'Auto fetch settings saved.'))
    emit('update:modelValue', false)
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : String(error)
    emit('notify', errorMessage.value, true)
  } finally {
    busy.value = false
  }
}

async function fetchNow(): Promise<void> {
  if (!canFetch.value) return
  fetching.value = true
  errorMessage.value = ''
  try {
    if (!preview) {
      if (dirty.value) await save()
      const result = await props.cli.fetchRemoteKeybox()
      emit('notify', result === 'installed'
        ? tr('keybox_auto_fetch_installed', 'Remote Keybox downloaded and installed.')
        : tr('keybox_auto_fetch_unchanged', 'The remote Keybox already matches the installed one.'))
      emit('changed')
    } else {
      emit('notify', tr('keybox_auto_fetch_unchanged', 'The remote Keybox already matches the installed one.'))
    }
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : String(error)
    emit('notify', errorMessage.value, true)
  } finally {
    fetching.value = false
  }
}
</script>

<template>
  <MiuixDialog
    :model-value="modelValue"
    :title="tr('tools_keybox_auto_fetch', 'Auto fetch Keybox')"
    :close-on-click-modal="!anyBusy"
    @update:model-value="value => { if (!value) requestClose() }"
  >
    <div class="remote-dialog" :aria-busy="anyBusy || status === 'loading'">
      <p class="remote-dialog__warning">
        {{ tr('keybox_auto_fetch_warning', 'When enabled, a base64-encoded keybox.xml is downloaded over HTTPS from the URL below after boot and then at the configured interval, and replaces the installed Keybox when it differs. Only use a source you trust.') }}
      </p>

      <div v-if="status === 'loading'" class="remote-dialog__loading" role="status">
        <MiuixProgressIndicator type="circular" :size="28" />
        <span>{{ tr('home_status_loading', 'Checking') }}</span>
      </div>
      <template v-else-if="status === 'ready'">
        <MiuixCard class="remote-dialog__card" press-feedback="none">
          <MiuixSwitchPreference
            v-model="enabled"
            :title="tr('keybox_auto_fetch_enabled', 'Enable auto fetch')"
            :summary="tr('keybox_auto_fetch_enabled_desc', 'Disabled by default. Changes are saved only after you tap Apply.')"
            :disabled="anyBusy"
          />
        </MiuixCard>
        <label class="remote-dialog__field">
          <span>{{ tr('keybox_auto_fetch_url', 'Keybox URL') }}</span>
          <input
            v-model="url"
            class="remote-dialog__input"
            type="url"
            inputmode="url"
            autocomplete="off"
            spellcheck="false"
            :placeholder="DEFAULT_KEYBOX_REMOTE_URL"
            :disabled="anyBusy"
          >
          <small>{{ tr('keybox_auto_fetch_url_desc', 'Leave empty to use the default source.') }}</small>
        </label>
        <label class="remote-dialog__field">
          <span>{{ tr('keybox_auto_fetch_proxy', 'Download proxy') }}</span>
          <input
            v-model="proxy"
            class="remote-dialog__input"
            type="url"
            inputmode="url"
            autocomplete="off"
            spellcheck="false"
            :disabled="anyBusy"
          >
          <small>{{ tr('keybox_auto_fetch_proxy_desc', 'Optional HTTPS proxy template. The %s placeholder is replaced by the Keybox URL, for example: https://gh-proxy.org/$url. Leave empty to fetch directly.', '$url') }}</small>
        </label>
        <div class="remote-dialog__field">
          <span>{{ tr('keybox_auto_fetch_interval', 'Update interval') }}</span>
          <div class="remote-dialog__interval">
            <label class="remote-dialog__interval-part">
              <input
                v-model="intervalHours"
                class="remote-dialog__input"
                type="number"
                inputmode="numeric"
                min="0"
                max="168"
                autocomplete="off"
                :disabled="anyBusy"
              >
              <small>{{ tr('keybox_auto_fetch_interval_hours', 'Hours') }}</small>
            </label>
            <label class="remote-dialog__interval-part">
              <input
                v-model="intervalMinutes"
                class="remote-dialog__input"
                type="number"
                inputmode="numeric"
                min="0"
                max="59"
                autocomplete="off"
                :disabled="anyBusy"
              >
              <small>{{ tr('keybox_auto_fetch_interval_minutes', 'Minutes') }}</small>
            </label>
          </div>
          <small>{{ tr('keybox_auto_fetch_interval_desc', 'The fetch repeats at this interval, from 15 minutes up to 7 days. Changes apply from the next refresh cycle.') }}</small>
        </div>
      </template>

      <p v-if="status === 'ready' && (!isHttpsOrEmpty(url.trim()) || !isHttpsOrEmpty(proxy.trim()))" class="remote-dialog__error" role="alert">
        {{ tr('keybox_auto_fetch_invalid_url', 'The Keybox URL and proxy must be empty or start with https://') }}
      </p>
      <p v-else-if="status === 'ready' && !intervalValid" class="remote-dialog__error" role="alert">
        {{ tr('keybox_auto_fetch_invalid_interval', 'The update interval must be between 15 minutes and 7 days.') }}
      </p>
      <p v-else-if="errorMessage" class="remote-dialog__error" role="alert">{{ errorMessage }}</p>
      <p v-if="preview" class="remote-dialog__preview" role="status">
        {{ tr('keybox_auto_fetch_preview', 'Preview only. Device settings cannot be saved here.') }}
      </p>

      <div class="remote-dialog__actions">
        <MiuixButton :disabled="anyBusy" @click="requestClose">
          {{ tr('functional_button_cancel', 'Cancel') }}
        </MiuixButton>
        <MiuixButton :disabled="!canFetch" @click="fetchNow">
          <MiuixProgressIndicator v-if="fetching" type="circular" :size="18" />
          {{ tr('keybox_auto_fetch_now', 'Fetch now') }}
        </MiuixButton>
        <MiuixButton v-if="status === 'error'" type="primary" @click="load">
          {{ tr('functional_button_retry', 'Retry') }}
        </MiuixButton>
        <MiuixButton v-else type="primary" :disabled="!canApply" @click="apply">
          <MiuixProgressIndicator v-if="busy" type="circular" :size="18" />
          {{ tr('functional_button_apply', 'Apply') }}
        </MiuixButton>
      </div>
    </div>
  </MiuixDialog>
</template>

<style scoped>
.remote-dialog { display: flex; flex-direction: column; gap: 14px; }
.remote-dialog p { margin: 0; color: var(--m-color-on-surface-variant-summary); font-size: 14px; line-height: 1.5; overflow-wrap: anywhere; }
.remote-dialog .remote-dialog__warning { color: var(--m-color-on-surface); }
.remote-dialog .remote-dialog__error { color: var(--m-color-error); }
.remote-dialog__loading { display: flex; min-height: 64px; align-items: center; justify-content: center; gap: 12px; color: var(--m-color-on-surface-variant-summary); }
.remote-dialog__card { padding: 8px 0; }
.remote-dialog__field { display: flex; flex-direction: column; gap: 6px; }
.remote-dialog__field > span { font-size: 14px; color: var(--m-color-on-surface); }
.remote-dialog__field > small { font-size: 12px; line-height: 1.45; color: var(--m-color-on-surface-variant-summary); overflow-wrap: anywhere; }
.remote-dialog__input {
  width: 100%;
  box-sizing: border-box;
  padding: 11px 14px;
  border: 1px solid var(--m-color-surface-container-high);
  border-radius: 12px;
  background: var(--m-color-surface-container);
  color: var(--m-color-on-surface);
  font-size: 14px;
}
.remote-dialog__input:focus { border-color: var(--m-color-primary); outline: none; }
.remote-dialog__input:disabled { opacity: 0.6; }
.remote-dialog__input::placeholder { color: var(--m-color-on-surface-variant-summary); opacity: 0.75; }
.remote-dialog__interval { display: flex; gap: 12px; }
.remote-dialog__interval-part { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 6px; }
.remote-dialog__interval-part > small { font-size: 12px; color: var(--m-color-on-surface-variant-summary); }
.remote-dialog__interval-part input::-webkit-outer-spin-button,
.remote-dialog__interval-part input::-webkit-inner-spin-button { -webkit-appearance: none; margin: 0; }
.remote-dialog__interval-part input[type="number"] { -moz-appearance: textfield; appearance: textfield; }
.remote-dialog__actions { display: flex; gap: 12px; }
.remote-dialog__actions > * { flex: 1; min-width: 0; }
.remote-dialog :deep(.m-basic-component) { padding: 8px 0; }
.remote-dialog :deep(.m-basic-component__center > .m-text--headline1) { font-size: 16px; line-height: 1.35; }
.remote-dialog :deep(.m-basic-component__center > .m-text--body2) { font-size: 14px; line-height: 1.4; }
</style>
