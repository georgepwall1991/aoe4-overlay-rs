// Smoke test: connect to the app's OBS websocket and print received messages.
const ws = new WebSocket('ws://127.0.0.1:7307');
const msgs = [];
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  msgs.push(m.type);
  if (m.type === 'player_data') {
    console.log('player_data: map=%s players=%d first=%s (%s, rating %s)',
      m.data.map, m.data.players.length, m.data.players[0]?.name,
      m.data.players[0]?.civ, m.data.players[0]?.rating);
  } else {
    console.log(m.type + ': ' + JSON.stringify(m.data));
  }
};
ws.onerror = (e) => { console.log('WS ERROR', e.message); process.exit(1); };
setTimeout(() => {
  console.log('received message types:', msgs.join(', ') || 'NONE');
  process.exit(msgs.includes('color') && msgs.includes('player_data') ? 0 : 2);
}, 25000);
