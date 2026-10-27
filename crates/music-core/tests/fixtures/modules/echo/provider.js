// Echo: a test provider for music-macos. It resolves songs to the audio files
// listed in `catalog.json` on a server you configure (see the module tests and
// `examples/module_server.rs`). Not a real service.
//
// catalog.json: [{id, title, artists, album, durationMs, explicit,
//                 variants: [{quality, file, mimeType, codec, sampleRate, bitrate}]}]

function settingValue(context, key) {
  const field = context && context.settings && context.settings[key];
  return field ? field.value : null;
}

function requestHeaders(context) {
  const token = settingValue(context, 'apiToken');
  return token ? {Authorization: 'Bearer ' + token} : {};
}

function baseUrl(context) {
  const base = settingValue(context, 'baseUrl');
  if (!base) throw new Error('Server URL is not configured');
  return base.endsWith('/') ? base : base + '/';
}

async function loadCatalog(context) {
  const response = await fetch(new URL('catalog.json', baseUrl(context)).href, {headers: requestHeaders(context)});
  if (!response.ok) throw new Error('catalog request failed: HTTP ' + response.status);
  const entries = await response.json();
  if (!Array.isArray(entries)) throw new TypeError('catalog.json must be an array');
  return entries;
}

function words(text) {
  return String(text).toLowerCase().normalize('NFKD').replace(/[̀-ͯ]/g, '')
    .split(/[^\p{L}\p{N}]+/u).filter(Boolean);
}

async function searchTracks(query, limit, context) {
  const wanted = words(query);
  const max = Math.min(limit, Number(settingValue(context, 'searchLimit')) || limit);
  const entries = await loadCatalog(context);
  console.log('search', JSON.stringify(query), 'in', entries.length, 'entries');
  return entries
    .filter(entry => {
      const have = new Set(words(entry.title + ' ' + (entry.artists || []).join(' ')));
      return wanted.every(word => have.has(word));
    })
    .slice(0, max)
    .map(entry => ({
      id: entry.id, title: entry.title, artists: entry.artists, album: entry.album,
      durationMs: entry.durationMs, explicit: entry.explicit,
    }));
}

async function getTrackStreamUrl(trackId, quality, context) {
  const entries = await loadCatalog(context);
  const entry = entries.find(item => item.id === trackId);
  if (!entry) throw new Error('unknown track ' + trackId);
  const lossless = settingValue(context, 'preferLossless') !== false && quality !== 'compressed';
  const wanted = lossless ? 'lossless' : 'compressed';
  const variant = entry.variants.find(v => v.quality === wanted) || entry.variants[0];
  if (!variant) throw new Error('no audio for ' + trackId);
  return {
    url: new URL('audio/' + encodeURIComponent(variant.file), baseUrl(context)).href,
    headers: requestHeaders(context),
    mimeType: variant.mimeType,
    codec: variant.codec,
    sampleRate: variant.sampleRate,
    bitrate: variant.bitrate || 0,
    contentLength: variant.contentLength || 0,
    variant: variant.quality + ':' + variant.file,
    expiresInSeconds: 600,
  };
}

module.exports = { searchTracks, getTrackStreamUrl };
