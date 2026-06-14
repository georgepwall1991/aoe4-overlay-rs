// Quick syntax check for the inline <script> in ui/index.html.
// Extracts every <script>…</script> body and compiles it with new Function,
// so a typo fails loudly instead of silently breaking the games page.
import { readFileSync } from 'node:fs';

const html = readFileSync(new URL('../ui/index.html', import.meta.url), 'utf8');
const re = /<script(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/gi;
let m, n = 0, bytes = 0;
while ((m = re.exec(html))) {
  const body = m[1];
  n++; bytes += body.length;
  try {
    // eslint-disable-next-line no-new-func
    new Function(body);
  } catch (e) {
    console.error(`✗ script block #${n} failed to compile: ${e.message}`);
    process.exit(1);
  }
}
console.log(`✓ ${n} inline script block(s) compiled cleanly (${bytes} bytes)`);
