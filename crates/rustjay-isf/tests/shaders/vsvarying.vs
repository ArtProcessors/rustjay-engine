varying vec2 marker;

void main()
{
	isf_vertShaderInit();
	// Reads a uniform and an input, both of which the vertex stage must see.
	marker = vec2(scale, 128.0 / RENDERSIZE.y);
}
