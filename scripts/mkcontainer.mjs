// Create an Azure blob container in Azurite (idempotent). The well-known
// emulator account/key — public development credentials, not a secret.
import crypto from 'node:crypto';

const account = 'devstoreaccount1';
const key = 'Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==';
const container = process.argv[2] || 'fragment-dev';
const port = process.argv[3] || '10000';

const date = new Date().toUTCString();
const version = '2020-04-08';
const stringToSign = [
  'PUT', '', '', '', '', '', '', '', '', '', '', '',
  `x-ms-date:${date}`,
  `x-ms-version:${version}`,
  `/${account}/${account}/${container}\nrestype:container`,
].join('\n');

const sig = crypto.createHmac('sha256', Buffer.from(key, 'base64')).update(stringToSign, 'utf8').digest('base64');
const res = await fetch(`http://127.0.0.1:${port}/${account}/${container}?restype=container`, {
  method: 'PUT',
  headers: {
    'x-ms-date': date,
    'x-ms-version': version,
    'Content-Length': '0',
    Authorization: `SharedKey ${account}:${sig}`,
  },
});
if (res.status === 201) console.log(`created container ${container}`);
else if (res.status === 409) console.log(`container ${container} already exists`);
else {
  console.error(`unexpected ${res.status}: ${await res.text()}`);
  process.exit(1);
}
