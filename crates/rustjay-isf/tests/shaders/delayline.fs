/*{
	"DESCRIPTION": "test: a PERSISTENT pass target read back one frame later",
	"ISFVSN": "2.0",
	"INPUTS": [
		{ "NAME": "inputImage", "TYPE": "image" }
	],
	"PASSES": [
		{ "TARGET": "buf1", "PERSISTENT": true },
		{}
	]
}*/

void main()
{
	if (PASSINDEX == 0) {
		gl_FragColor = IMG_THIS_NORM_PIXEL(inputImage);
	} else {
		gl_FragColor = IMG_THIS_NORM_PIXEL(buf1);
	}
}
