/*
{
    "CATEGORIES": [
        "Blur",
        "Shadertoy"
    ],
    "DESCRIPTION": "Golden-angle bokeh blur of the layer below. Ported for KOVVBOJ from https://www.shadertoy.com/view/4tjfWd by battlebottle (modified from https://www.shadertoy.com/view/4d2Xzw); the original's London photo, fake street lamps and ACES tonemap are dropped.",
    "INPUTS": [
        {
            "NAME": "inputImage",
            "TYPE": "image"
        },
        {
            "NAME": "blurRadius",
            "LABEL": "Blur Radius",
            "TYPE": "float",
            "DEFAULT": 0.8,
            "MIN": 0.0,
            "MAX": 2.0
        }
    ],
    "PASSES": [
        {
            "FLOAT": true,
            "TARGET": "BufferA"
        },
        {
        }
    ]
}
*/

#define GOLDEN_ANGLE 2.39996
// ponytail: samples = ITERATIONS * radius^2, so MAX 2.0 caps it at 512 per pixel;
// raise MAX only with a lower ITERATIONS or a downsampled BufferA.
#define ITERATIONS 128

const float SRGB_GAMMA = 1.0 / 2.2;

void main() {
	if (PASSINDEX == 0)	{
		// Linearise once so the blur averages light, not gamma-encoded values.
		vec2 uv = gl_FragCoord.xy / RENDERSIZE.xy;
		gl_FragColor = vec4(pow(IMG_NORM_PIXEL(inputImage, uv).rgb, vec3(1.0 / SRGB_GAMMA)), 1.0);
	}
	else if (PASSINDEX == 1)	{
		vec2 uv = gl_FragCoord.xy / RENDERSIZE.xy;
		mat2 rot = mat2(cos(GOLDEN_ANGLE), sin(GOLDEN_ANGLE), -sin(GOLDEN_ANGLE), cos(GOLDEN_ANGLE));
		// Circle in pixels, not UV, so the bokeh stays round at any aspect.
		vec2 aspect = vec2(RENDERSIZE.y / RENDERSIZE.x, 1.0);
		int iters = int(max(1.0, float(ITERATIONS) * blurRadius * blurRadius));
		vec2 vangle = vec2(0.0, blurRadius * 0.01 / sqrt(float(ITERATIONS)));
		vec3 acc = vec3(0.0);
		float r = 1.0;
		for (int j = 0; j < iters; j++) {
			r += 1.0 / r;
			vangle = rot * vangle;
			acc += IMG_NORM_PIXEL(BufferA, uv + (r - 1.0) * vangle * aspect).rgb;
		}
		gl_FragColor = vec4(pow(acc / float(iters), vec3(SRGB_GAMMA)), 1.0);
	}
}
