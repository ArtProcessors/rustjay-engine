varying vec2 passcoord;

void main()
{
	isf_vertShaderInit();
	passcoord = isf_FragNormCoord;
}
