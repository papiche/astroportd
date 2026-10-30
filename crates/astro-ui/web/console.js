'use strict';
// Console SSH de l'interface : un terminal xterm.js relié par WebSocket au `ssh` que la station lance vers l'ami aidé.
// Le ticket (30 s, un seul usage) arrive dans le fragment de l'adresse, qu'on efface aussitôt.
import { Terminal } from '/ui/xterm.js';
import { FitAddon } from '/ui/xterm-fit.js';

const $ = id => document.getElementById(id);
const params = new URLSearchParams(location.hash.slice(1));
const ticket = params.get('ticket') || '', label = params.get('label') || '';
history.replaceState(null, '', location.pathname);
if (label) { $('who').textContent = label; document.title = `Console — ${label}`; }
const state = (text, err) => { $('state').textContent = text; $('state').className = err ? 'err' : ''; };

if (!/^[0-9a-f]{48}$/.test(ticket)) {
  state('Ticket absent : ouvrez la console depuis Options › Assistance.', true);
} else {
  const term = new Terminal({ cursorBlink: true, scrollback: 5000, fontFamily: 'ui-monospace, Menlo, Consolas, monospace', fontSize: 14, theme: { background: '#0b0d12' } });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open($('term'));
  fit.fit();
  const ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/api/v1/station/console?ticket=${ticket}`);
  ws.binaryType = 'arraybuffer';
  const size = () => { fit.fit(); if (ws.readyState === 1) ws.send(JSON.stringify({ resize: [term.cols, term.rows] })); };
  ws.onopen = () => { state('Connecté'); $('close').hidden = false; size(); term.focus(); };
  ws.onmessage = ev => term.write(new Uint8Array(ev.data));
  ws.onclose = () => { state('Session terminée'); $('close').hidden = true; term.options.disableStdin = true; };
  ws.onerror = () => state('Connexion refusée (ticket périmé ou déjà utilisé ?)', true);
  const enc = new TextEncoder();
  term.onData(d => { if (ws.readyState === 1) ws.send(enc.encode(d)); });
  new ResizeObserver(size).observe($('term'));
  $('close').onclick = () => ws.close();
  addEventListener('pagehide', () => ws.close());
}
