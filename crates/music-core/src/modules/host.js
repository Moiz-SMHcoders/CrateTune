// Host bindings for provider modules (MODULES.md "Host APIs and limits").
// Ported from ArchiveTune modules/src/main/assets/module-host.js
// — © Rukamori and ArchiveTune contributors, GPL-3.0.
// Differences: fetch and timers settle through the native event loop
// (__moduleFetchStart / __moduleSettle, __moduleTimerStart / __moduleTimerFire);
// console output goes to the module's diagnostic log; `storage`, `utils` and
// `crypto.getRandomValues` / `crypto.randomUUID` are extra helpers.
(function() {
const native = {
  fetchStart: __moduleFetchStart, abort: __moduleAbort, timerStart: __moduleTimerStart,
  timerClear: __moduleClearTimer, url: __moduleUrl, log: __moduleLog, storage: __moduleStorage,
  digest: __moduleDigest, random: __moduleRandom, uuid: __moduleUuid,
};
const format = value => {
  if (typeof value === 'string') return value;
  if (value instanceof Error) return (value.name || 'Error') + ': ' + value.message;
  try { return JSON.stringify(value); } catch (_) { return String(value); }
};
const logger = level => (...args) => native.log(level, args.map(format).join(' '));
globalThis.console = Object.freeze({log: logger('log'), info: logger('info'), warn: logger('warn'),
  error: logger('error'), debug: logger('debug')});
globalThis.Headers = class Headers {
  constructor(init = {}) {
    this.values = Object.create(null);
    if (init instanceof Headers) init = init.values;
    if (Array.isArray(init)) init.forEach(([k,v]) => this.set(k,v));
    else Object.keys(init).forEach(k => this.set(k,init[k]));
  }
  set(key,value) { this.values[String(key).toLowerCase()] = String(value); }
  append(key,value) { this.set(key,this.has(key) ? this.get(key) + ', ' + value : value); }
  get(key) { return this.values[String(key).toLowerCase()] ?? null; }
  has(key) { return Object.prototype.hasOwnProperty.call(this.values,String(key).toLowerCase()); }
  delete(key) { delete this.values[String(key).toLowerCase()]; }
  entries() { return Object.entries(this.values)[Symbol.iterator](); }
  keys() { return Object.keys(this.values)[Symbol.iterator](); }
  forEach(fn,thisArg) { Object.entries(this.values).forEach(([k,v]) => fn.call(thisArg,v,k,this)); }
  [Symbol.iterator]() { return this.entries(); }
};
globalThis.URLSearchParams = class URLSearchParams {
  constructor(input = '') {
    this.pairs = [];
    if (input instanceof URLSearchParams) this.pairs = input.pairs.map(p => p.slice());
    else if (typeof input === 'string') {
      input.replace(/^\?/,'').split('&').filter(Boolean).forEach(p => {
        const i = p.indexOf('=');
        this.append(this.decode(i < 0 ? p : p.slice(0,i)),this.decode(i < 0 ? '' : p.slice(i+1)));
      });
    } else if (Array.isArray(input)) input.forEach(([k,v]) => this.append(k,v));
    else Object.keys(input).forEach(k => this.append(k,input[k]));
  }
  decode(s) { return decodeURIComponent(s.replace(/\+/g,' ')); }
  encode(s) { return encodeURIComponent(s).replace(/%20/g,'+').replace(/[!'()~]/g,c => '%' + c.charCodeAt(0).toString(16).toUpperCase()); }
  append(k,v) { this.pairs.push([String(k),String(v)]); }
  get(k) { return this.pairs.find(p => p[0] === String(k))?.[1] ?? null; }
  getAll(k) { return this.pairs.filter(p => p[0] === String(k)).map(p => p[1]); }
  has(k) { return this.pairs.some(p => p[0] === String(k)); }
  delete(k) { this.pairs = this.pairs.filter(p => p[0] !== String(k)); }
  set(k,v) { this.delete(k); this.append(k,v); }
  sort() { this.pairs.sort((a,b) => a[0].localeCompare(b[0])); }
  toString() { return this.pairs.map(([k,v]) => this.encode(k) + '=' + this.encode(v)).join('&'); }
  entries() { return this.pairs[Symbol.iterator](); }
  keys() { return this.pairs.map(p => p[0])[Symbol.iterator](); }
  values() { return this.pairs.map(p => p[1])[Symbol.iterator](); }
  forEach(fn,thisArg) { this.pairs.forEach(([k,v]) => fn.call(thisArg,v,k,this)); }
  [Symbol.iterator]() { return this.entries(); }
};
globalThis.URL = class URL {
  constructor(value,base) {
    this.parts = JSON.parse(native.url(String(value),base == null ? undefined : String(base)));
    this.searchParams = new URLSearchParams(this.parts.search);
    this.initialQuery = this.searchParams.toString();
  }
  get href() { return this.toString(); }
  set href(value) { this.parts = JSON.parse(native.url(String(value),this.toString())); this.searchParams = new URLSearchParams(this.parts.search); this.initialQuery = this.searchParams.toString(); }
  get origin() { return this.parts.origin; }
  get protocol() { return this.parts.protocol; }
  get hostname() { return this.parts.hostname; }
  get host() { return this.parts.host; }
  get pathname() { return this.parts.pathname; }
  set pathname(value) { this.parts.pathname = String(value).replace(/^([^/])/,'/$1'); }
  get search() { const value = this.searchParams.toString(); return value === this.initialQuery ? this.parts.search : value ? '?' + value : ''; }
  set search(value) { this.parts.search = value ? '?' + String(value).replace(/^\?/,'') : ''; this.searchParams = new URLSearchParams(this.parts.search); this.initialQuery = this.searchParams.toString(); }
  get hash() { return this.parts.hash; }
  set hash(value) { this.parts.hash = value ? '#' + String(value).replace(/^#/,'') : ''; }
  toString() { return this.origin + this.pathname + this.search + this.hash; }
  toJSON() { return this.toString(); }
};
globalThis.atob = function(value) {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  const text = String(value).replace(/\s/g,'').replace(/=+$/,'');
  if (text.length % 4 === 1) throw new TypeError('Invalid base64');
  let bits = 0, count = 0, result = '';
  for (const c of text) {
    const v = alphabet.indexOf(c);
    if (v < 0) throw new TypeError('Invalid base64');
    bits = (bits << 6) | v; count += 6;
    if (count >= 8) { count -= 8; result += String.fromCharCode((bits >>> count) & 255); }
  }
  return result;
};
globalThis.btoa = function(value) {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  let result = '', bits = 0, count = 0;
  for (const c of String(value)) {
    const v = c.charCodeAt(0);
    if (v > 255) throw new TypeError('Invalid binary string');
    bits = (bits << 8) | v; count += 8;
    while (count >= 6) { count -= 6; result += alphabet[(bits >>> count) & 63]; }
  }
  if (count) result += alphabet[(bits << (6-count)) & 63];
  while (result.length % 4) result += '=';
  return result;
};
globalThis.TextEncoder = class TextEncoder {
  encode(text = '') { return Uint8Array.from(unescape(encodeURIComponent(String(text))),c => c.charCodeAt(0)); }
};
globalThis.TextDecoder = class TextDecoder {
  decode(input = new Uint8Array()) {
    const bytes = input instanceof ArrayBuffer ? new Uint8Array(input) : new Uint8Array(input.buffer,input.byteOffset,input.byteLength);
    return decodeURIComponent(Array.from(bytes,b => '%' + b.toString(16).padStart(2,'0')).join(''));
  }
};
globalThis.AbortController = class AbortController {
  constructor() {
    this.signal = {aborted:false,listeners:new Set(),addEventListener(type,fn){if(type==='abort')this.listeners.add(fn);},removeEventListener(type,fn){this.listeners.delete(fn);},throwIfAborted(){if(this.aborted)throw new Error('Aborted');}};
  }
  abort() { if (!this.signal.aborted) { this.signal.aborted = true; this.signal.listeners.forEach(fn => fn()); } }
};
let timerSequence = 0;
const timers = new Map();
globalThis.setTimeout = function(fn,millis = 0,...args) {
  if (typeof fn !== 'function' || timers.size >= 128) throw new TypeError('Invalid timer');
  const id = ++timerSequence;
  timers.set(id,() => fn(...args));
  native.timerStart(id,Math.max(0,Math.min(Number(millis)||0,15000)));
  return id;
};
globalThis.clearTimeout = function(id) { if (timers.delete(id)) native.timerClear(Number(id)); };
globalThis.__moduleTimerFire = function(id) {
  const fn = timers.get(id);
  if (!fn) return;
  timers.delete(id);
  try { fn(); } catch (error) { console.error('timer callback failed:', error); }
};
globalThis.__moduleClearTimers = function() {
  for (const id of timers.keys()) native.timerClear(id);
  timers.clear();
};
const pending = new Map();
globalThis.__moduleSettle = function(id,ok,payload) {
  const waiter = pending.get(id);
  if (!waiter) return;
  pending.delete(id);
  if (ok) waiter.resolve(payload); else waiter.reject(new Error(payload || 'Request failed'));
};
const nativeFetch = request => new Promise((resolve,reject) => {
  pending.set(request.id,{resolve,reject});
  try { native.fetchStart(JSON.stringify(request)); }
  catch (error) { pending.delete(request.id); reject(error); }
});
globalThis.Blob = class Blob {
  constructor(parts = [],options = {}) {
    this.type = options.type || '';
    this.parts = parts.map(p => p instanceof Blob ? p.bytes() : typeof p === 'string' ? new TextEncoder().encode(p) : new Uint8Array(p.buffer || p,p.byteOffset || 0,p.byteLength));
    this.size = this.parts.reduce((n,p) => n+p.length,0);
  }
  bytes() { const out = new Uint8Array(this.size); let i = 0; this.parts.forEach(p => {out.set(p,i);i+=p.length;}); return out; }
  async arrayBuffer() { return this.bytes().buffer; }
  async text() { return new TextDecoder().decode(this.bytes()); }
};
globalThis.FormData = class FormData {
  constructor() { this.fields = []; }
  append(name,value,filename) { this.fields.push([String(name),value,filename]); }
  entries() { return this.fields.map(([k,v]) => [k,v])[Symbol.iterator](); }
  [Symbol.iterator]() { return this.entries(); }
};
let requestSequence = 0;
globalThis.fetch = async function(input,options = {}) {
  const headers = new Headers(options.headers || {});
  let body = options.body;
  if (body instanceof URLSearchParams) { body = body.toString(); if(!headers.has('content-type'))headers.set('content-type','application/x-www-form-urlencoded;charset=UTF-8'); }
  if (body instanceof FormData) {
    const boundary = '----ModuleBoundary' + (++requestSequence);
    const parts = [];
    body.fields.forEach(([name,value,filename]) => {
      if (/[\r\n"]/.test(name) || (filename && /[\r\n"]/.test(filename))) throw new TypeError('Invalid form field');
      parts.push('--' + boundary + '\r\nContent-Disposition: form-data; name="' + name + '"' + (value instanceof Blob ? '; filename="' + (filename || 'blob') + '"' : '') + '\r\n');
      if (value instanceof Blob) parts.push('Content-Type: ' + (value.type || 'application/octet-stream') + '\r\n');
      parts.push('\r\n',value instanceof Blob ? value : String(value),'\r\n');
    });
    parts.push('--' + boundary + '--\r\n');
    body = new Blob(parts);
    headers.set('content-type','multipart/form-data; boundary=' + boundary);
  }
  const request = {id:String(++requestSequence),url:String(input),method:options.method || 'GET',headers:headers.values};
  if (body instanceof Blob || body instanceof ArrayBuffer || ArrayBuffer.isView(body)) {
    const bytes = body instanceof Blob ? body.bytes() : body instanceof ArrayBuffer ? new Uint8Array(body) : new Uint8Array(body.buffer,body.byteOffset,body.byteLength);
    request.bodyBase64 = btoa(Array.from(bytes,b => String.fromCharCode(b)).join(''));
  } else if (body != null) request.body = String(body);
  const signal = options.signal;
  if (signal?.aborted) { const aborted = new Error('Aborted'); aborted.name = 'AbortError'; throw aborted; }
  const abort = () => native.abort(request.id);
  signal?.addEventListener('abort',abort);
  try {
    const raw = JSON.parse(await nativeFetch(request));
    return {ok:raw.ok,status:raw.status,statusText:raw.statusText,url:raw.url,headers:new Headers(raw.headers),
      async text(){return raw.text;},async json(){return JSON.parse(raw.text);},
      async arrayBuffer(){return Uint8Array.from(atob(raw.base64),c=>c.charCodeAt(0)).buffer;},
      async blob(){return new Blob([await this.arrayBuffer()],{type:this.headers.get('content-type')||''});},
      clone(){return this;}};
  } catch (error) {
    if (signal?.aborted) { const aborted = new Error('Aborted'); aborted.name = 'AbortError'; throw aborted; }
    throw error;
  } finally { signal?.removeEventListener('abort',abort); }
};
const storageCall = input => JSON.parse(native.storage(JSON.stringify(input)));
globalThis.storage = Object.freeze({
  get: key => storageCall({action:'get',key:String(key)}),
  set: (key,value) => storageCall({action:'set',key:String(key),value: value === undefined ? null : value}),
  remove: key => storageCall({action:'remove',key:String(key)}),
  clear: () => storageCall({action:'clear'}),
  keys: () => storageCall({action:'keys'}),
});
const bytesOf = value => typeof value === 'string' ? new TextEncoder().encode(value)
  : value instanceof ArrayBuffer ? new Uint8Array(value) : new Uint8Array(value.buffer,value.byteOffset,value.byteLength);
const base64Bytes = bytes => btoa(Array.from(bytes,b => String.fromCharCode(b)).join(''));
const digest = algorithm => value => native.digest(algorithm,base64Bytes(bytesOf(value)));
globalThis.utils = Object.freeze({
  sha256: digest('sha256'), sha1: digest('sha1'), md5: digest('md5'),
  uuid: () => native.uuid(),
  base64Encode: value => base64Bytes(bytesOf(String(value))),
  base64Decode: value => new TextDecoder().decode(Uint8Array.from(atob(String(value)),c => c.charCodeAt(0))),
  parseJSON: value => JSON.parse(value), stringifyJSON: value => JSON.stringify(value),
});
globalThis.crypto = Object.freeze({
  getRandomValues(array) {
    if (!ArrayBuffer.isView(array) || array.byteLength > 65536) throw new TypeError('Invalid array');
    const bytes = Uint8Array.from(atob(native.random(array.byteLength)),c => c.charCodeAt(0));
    new Uint8Array(array.buffer,array.byteOffset,array.byteLength).set(bytes);
    return array;
  },
  randomUUID: () => native.uuid(),
});
for (const name of Object.keys(native)) delete globalThis['__module' + name[0].toUpperCase() + name.slice(1)];
delete globalThis.__moduleFetchStart; delete globalThis.__moduleClearTimer;
})();
"module-host-ready";
