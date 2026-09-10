/*{
	"DESCRIPTION": "test: a single PERSISTENT pass at half size, sampled by the pass that writes it — feedback, and the last pass still reaches the screen",
	"ISFVSN": "2.0",
	"INPUTS": [
		{ "NAME": "inputImage", "TYPE": "image" }
	],
	"PASSES": [
		{ "TARGET": "bufA", "PERSISTENT": true, "WIDTH": "$WIDTH/2.0", "HEIGHT": "floor($HEIGHT/2.0)" }
	]
}*/

void main()
{
	vec4 fresh = IMG_THIS_NORM_PIXEL(inputImage);
	vec4 stale = IMG_THIS_NORM_PIXEL(bufA);
	gl_FragColor = mix(fresh, stale, 0.5);
}
