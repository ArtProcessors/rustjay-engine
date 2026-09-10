/*{
	"DESCRIPTION": "test: sampling through a coordinate the companion .vs computed — same corners as imgpassthrough, so a flipped vertex stage fails",
	"ISFVSN": "2.0",
	"INPUTS": [
		{ "NAME": "inputImage", "TYPE": "image" }
	]
}*/

varying vec2 passcoord;

void main()
{
	gl_FragColor = IMG_NORM_PIXEL(inputImage, passcoord);
}
