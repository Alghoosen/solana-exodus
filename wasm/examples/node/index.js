const util = require('util');

const sdk = require('./../..');

const TextEncoder = util.TextEncoder;

(async () => {
  const keyPair = await sdk.ed25519.keypair.generate();
  const signature = await sdk.ed25519.sign(
    keyPair.publicKey,
    keyPair.secretKey,
    new TextEncoder().encode('message to encode'),
  );
  console.log('KeyPair and Signature', {
    keyPair,
    signature,
  });

  console.log(
    'KeyPair from secret key',
    await sdk.ed25519.keypair.fromSecretKey(keyPair.secretKey),
  );

  console.log('Sha256', await sdk.hasher.sha256(signature));
})();
