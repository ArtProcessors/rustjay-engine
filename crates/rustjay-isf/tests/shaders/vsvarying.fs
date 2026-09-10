/*{
	"DESCRIPTION": "test: a varying written by the companion .vs",
	"ISFVSN": "2.0",
	"INPUTS": [
		{ "NAME": "inputImage", "TYPE": "image" },
		{ "NAME": "scale", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 }
	]
}*/

varying vec2 marker;

void main()
{
	gl_FragColor = vec4(marker, 0.0, 1.0);
}
