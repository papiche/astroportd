// Classement de l'annuaire des stations (/stations) : la plus proche qui accueille d'abord.
import assert from 'node:assert/strict';
import { rank, canHost, freeSeats, distanceKm } from '../web/stations.js';

const toulouse = { lat: 43.60, lon: 1.44 };
const st = (name, o) => ({ station: name, name, open: true, used: 0, total: 0, join: 'https://' + name + '/register', at: 100, lat: null, lon: null, ...o });

assert.ok(Math.abs(distanceKm(toulouse, { lat: 48.86, lon: 2.35 }) - 588) < 5, 'Toulouse–Paris ≈ 588 km');
assert.equal(freeSeats(st('a', { used: 3, total: 0 })), Infinity, '0 = sans limite');
assert.equal(canHost(st('b', { used: 5, total: 5 })), false, 'complète');
assert.equal(canHost(st('c', { join: null })), false, 'sans adresse publique, injoignable depuis ailleurs');
assert.equal(canHost(st('d', { join: null, self: true })), true, 'celle-ci reste joignable');

const list = [
  st('paris', { lat: 48.86, lon: 2.35, used: 1, total: 500 }),
  st('albi', { lat: 43.93, lon: 2.15, used: 499, total: 500 }),
  st('muret', { lat: 43.46, lon: 1.33, used: 10, total: 10 }), // la plus proche, mais complète
  st('sans-gps', { used: 0, total: 0 }),
  st('fermee', { lat: 43.6, lon: 1.44, open: false }),
];
const near = rank(list, toulouse).map(s => s.name);
assert.deepEqual(near, ['albi', 'paris', 'sans-gps', 'fermee', 'muret'], 'proches qui accueillent, puis sans position, puis celles qui n’accueillent pas (par distance)');
const blind = rank(list, null).map(s => s.name);
assert.equal(blind[0], 'sans-gps', 'sans position du visiteur : la plus libre');
assert.equal(rank([st('x', { lat: 43.6, lon: 1.44, used: 9, total: 10 }), st('y', { lat: 43.62, lon: 1.45, used: 1, total: 10 })], toulouse)[0].name, 'y', 'à moins de 5 km près, la plus libre');
console.log('stations_rank : ok');
