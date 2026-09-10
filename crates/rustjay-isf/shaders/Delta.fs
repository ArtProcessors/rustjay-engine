/*{
	"DESCRIPTION": "RGB delay / motion extraction (Posy's method). Differences the input against itself a few frames ago, per channel. Port of the `delta` example, using PERSISTENT passes for the frame history.",
	"CREDIT": "rustjay-engine",
	"CATEGORIES": ["Motion", "Glitch"],
	"ISFVSN": "2.0",
	"INPUTS": [
		{ "NAME": "inputImage", "TYPE": "image" },
		{ "NAME": "red_delay",   "LABEL": "Red Delay",   "TYPE": "long", "VALUES": [0,1,2,3,4], "LABELS": ["0","1","2","3","4"], "DEFAULT": 0 },
		{ "NAME": "green_delay", "LABEL": "Green Delay", "TYPE": "long", "VALUES": [0,1,2,3,4], "LABELS": ["0","1","2","3","4"], "DEFAULT": 2 },
		{ "NAME": "blue_delay",  "LABEL": "Blue Delay",  "TYPE": "long", "VALUES": [0,1,2,3,4], "LABELS": ["0","1","2","3","4"], "DEFAULT": 4 },
		{ "NAME": "intensity", "LABEL": "Intensity", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 1.0 },
		{ "NAME": "blend_mode", "LABEL": "Blend Mode", "TYPE": "long",
		  "VALUES": [0,1,2,3,4,5,6,7],
		  "LABELS": ["Replace","Add","Multiply","Screen","Difference","Overlay","Lighten","Darken"],
		  "DEFAULT": 0 },
		{ "NAME": "grayscale_input", "LABEL": "Grayscale Input", "TYPE": "bool", "DEFAULT": true },
		{ "NAME": "red_gain",   "LABEL": "Red Gain",   "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 1.0 },
		{ "NAME": "green_gain", "LABEL": "Green Gain", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 1.0 },
		{ "NAME": "blue_gain",  "LABEL": "Blue Gain",  "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 1.0 },
		{ "NAME": "input_mix",  "LABEL": "Input Mix",  "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 },
		{ "NAME": "trail_fade", "LABEL": "Trail Fade", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 },
		{ "NAME": "threshold",  "LABEL": "Threshold",  "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 },
		{ "NAME": "smoothing",  "LABEL": "Smoothing",  "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 }
	],
	"PASSES": [
		{ "TARGET": "buf1", "PERSISTENT": true },
		{ "TARGET": "buf2", "PERSISTENT": true },
		{ "TARGET": "buf3", "PERSISTENT": true },
		{ "TARGET": "buf4", "PERSISTENT": true },
		{}
	]
}*/

// Frame history. A persistent target is read one frame after it is written, so
// each buffer hands its content on to the next one and buf4 is four frames old.
// The engine's `delta` example rings 16 frames; here each frame of delay costs a
// pass and a buffer, so the line stops at four.

vec4 tap(int d, vec2 uv) {
	if (d <= 0) { return IMG_NORM_PIXEL(inputImage, uv); }
	if (d == 1) { return IMG_NORM_PIXEL(buf1, uv); }
	if (d == 2) { return IMG_NORM_PIXEL(buf2, uv); }
	if (d == 3) { return IMG_NORM_PIXEL(buf3, uv); }
	return IMG_NORM_PIXEL(buf4, uv);
}

float rgb_to_luma(vec3 rgb) {
	return dot(rgb, vec3(0.299, 0.587, 0.114));
}

vec3 blend_colors(vec3 base, vec3 layer, int mode) {
	if (mode == 1) { return min(base + layer, vec3(1.0)); }
	if (mode == 2) { return base * layer; }
	if (mode == 3) { return 1.0 - (1.0 - base) * (1.0 - layer); }
	if (mode == 4) { return abs(base - layer); }
	if (mode == 5) {
		return mix(2.0 * base * layer,
		           1.0 - 2.0 * (1.0 - base) * (1.0 - layer),
		           step(vec3(0.5), base));
	}
	if (mode == 6) { return max(base, layer); }
	if (mode == 7) { return min(base, layer); }
	return layer;
}

// Soft threshold: suppresses values below threshold and renormalizes the rest.
vec3 apply_threshold(vec3 color, float t) {
	if (t <= 0.0) { return color; }
	float k = clamp(t, 0.0, 0.99);
	return max(color - vec3(k), vec3(0.0)) / (1.0 - k);
}

// Negative gain inverts the signal instead of clamping to zero.
float apply_channel_gain(float value, float gain) {
	if (gain >= 0.0) { return clamp(value * gain, 0.0, 1.0); }
	return clamp((1.0 - value) * -gain, 0.0, 1.0);
}

// Core motion extraction: differences adjacent history frames per channel.
// Static content cancels out; only change energy passes through.
vec3 extract_motion(vec2 uv) {
	vec4 cur = tap(0, uv);
	vec4 s0 = tap(red_delay, uv);
	vec4 s1 = tap(green_delay, uv);
	vec4 s2 = tap(blue_delay, uv);

	vec3 motion;
	if (grayscale_input) {
		float cl = rgb_to_luma(cur.rgb);
		float l0 = rgb_to_luma(s0.rgb);
		float l1 = rgb_to_luma(s1.rgb);
		float l2 = rgb_to_luma(s2.rgb);
		motion = vec3(abs(l0 - l1), abs(l1 - l2), abs(cl - l2));
	} else {
		motion = vec3(abs(s0.r - s1.r), abs(s1.g - s2.g), abs(cur.b - s2.b));
	}

	motion.r = apply_channel_gain(motion.r, red_gain);
	motion.g = apply_channel_gain(motion.g, green_gain);
	motion.b = apply_channel_gain(motion.b, blue_gain);
	return motion;
}

void main() {
	vec2 uv = isf_FragNormCoord;

	if (PASSINDEX == 0) {
		gl_FragColor = IMG_NORM_PIXEL(inputImage, uv);
	} else if (PASSINDEX == 1) {
		gl_FragColor = IMG_NORM_PIXEL(buf1, uv);
	} else if (PASSINDEX == 2) {
		gl_FragColor = IMG_NORM_PIXEL(buf2, uv);
	} else if (PASSINDEX == 3) {
		gl_FragColor = IMG_NORM_PIXEL(buf3, uv);
	} else {
		vec3 motion = apply_threshold(extract_motion(uv), threshold);

		// Spatial smoothing: average four cardinal neighbours with the centre.
		if (smoothing > 0.0) {
			float off = smoothing * 0.01;
			vec3 smoothed = 0.25 * (
				extract_motion(uv + vec2(off, 0.0)) +
				extract_motion(uv - vec2(off, 0.0)) +
				extract_motion(uv + vec2(0.0, off)) +
				extract_motion(uv - vec2(0.0, off)));
			motion = mix(motion, smoothed, smoothing);
		}

		vec3 cur = tap(0, uv).rgb;
		vec3 blended = blend_colors(cur, motion, blend_mode);
		vec3 outc = mix(cur * input_mix, blended, intensity);

		// Trail fade: gamma-like boost that lifts dim motion trails.
		if (trail_fade > 0.0) {
			outc = pow(outc, vec3(1.0 - trail_fade * 0.5));
		}
		gl_FragColor = vec4(outc, 1.0);
	}
}
