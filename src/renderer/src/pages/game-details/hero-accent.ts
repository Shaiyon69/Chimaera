import Color from "color";

const BINS = 24;

/**
 * The banner's most prominent vivid hue as a readable accent (HSL lightness 70),
 * from RGBA pixels. Each pixel votes for its hue weighted by how colourful it is,
 * so a big dark sky does not drown out the art's real colour the way an average does.
 * Null when the art has no colour to speak of.
 */
export function pickAccent(rgba: Uint8ClampedArray): string | null {
  const weight = new Array<number>(BINS).fill(0);
  const hueSum = new Array<number>(BINS).fill(0);
  const satSum = new Array<number>(BINS).fill(0);
  let total = 0;

  for (let i = 0; i < rgba.length; i += 4) {
    const [r, g, b] = [rgba[i] / 255, rgba[i + 1] / 255, rgba[i + 2] / 255];
    const max = Math.max(r, g, b);
    const min = Math.min(r, g, b);
    const chroma = max - min;
    // Near-black and near-white pixels carry no usable hue.
    if (chroma < 0.06 || max < 0.08) continue;

    let hue: number;
    if (max === r) hue = ((g - b) / chroma + 6) % 6;
    else if (max === g) hue = (b - r) / chroma + 2;
    else hue = (r - g) / chroma + 4;
    hue *= 60;

    const w = chroma * chroma;
    const bin = Math.floor(hue / (360 / BINS)) % BINS;
    weight[bin] += w;
    hueSum[bin] += hue * w;
    satSum[bin] += (chroma / max) * w;
    total += w;
  }

  // shortcut: absolute threshold tuned for a 24x24 sample; retune if the sample size changes.
  if (total < 0.5) return null;

  // A hue split across a bin edge still wins: score each bin with its neighbours.
  let best = 0;
  let bestScore = -1;
  for (let bin = 0; bin < BINS; bin++) {
    const score =
      weight[bin] +
      0.5 * (weight[(bin + 1) % BINS] + weight[(bin + BINS - 1) % BINS]);
    if (score > bestScore) [best, bestScore] = [bin, score];
  }

  const hue = hueSum[best] / weight[best];
  const saturation = Math.min(
    85,
    Math.max(50, (satSum[best] / weight[best]) * 100)
  );
  return Color.hsl(hue, saturation, 70).hex();
}
