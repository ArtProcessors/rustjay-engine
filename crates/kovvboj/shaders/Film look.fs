/*{
    "DESCRIPTION": "One-shot Film Look + Grain + Flicker + Halation",
    "CREDIT": "ChatGPT",
    "ISFVSN": "2",
    "CATEGORIES": [ "Color" ],
    "INPUTS": [
        { "NAME": "inputImage", "TYPE": "image" },

        { "NAME": "contrast", "TYPE": "float", "DEFAULT": 1.2, "MIN": 0.5, "MAX": 2.0 },
        { "NAME": "fade", "TYPE": "float", "DEFAULT": 0.08, "MIN": 0.0, "MAX": 0.3 },
        { "NAME": "saturation", "TYPE": "float", "DEFAULT": 0.9, "MIN": 0.0, "MAX": 2.0 },

        { "NAME": "tealOrange", "TYPE": "float", "DEFAULT": 0.25, "MIN": 0.0, "MAX": 1.0 },
        { "NAME": "highlightRoll", "TYPE": "float", "DEFAULT": 0.3, "MIN": 0.0, "MAX": 1.0 },

        { "NAME": "gamma", "TYPE": "float", "DEFAULT": 0.95, "MIN": 0.5, "MAX": 2.0 },

        { "NAME": "grainAmount", "TYPE": "float", "DEFAULT": 0.08, "MIN": 0.0, "MAX": 0.3 },
        { "NAME": "grainSize", "TYPE": "float", "DEFAULT": 0.6, "MIN": 0.2, "MAX": 1.5 },

        { "NAME": "chromaShift", "TYPE": "float", "DEFAULT": 0.0015, "MIN": 0.0, "MAX": 0.01 },

        { "NAME": "sepiaAmount", "TYPE": "float", "DEFAULT": 0.0, "MIN": 0.0, "MAX": 1.0 },
        { "NAME": "posterizeFPS", "TYPE": "float", "DEFAULT": 0.0, "MIN": 0.0, "MAX": 60.0 },
        { "NAME": "flickerAmount", "TYPE": "float", "DEFAULT": 0.05, "MIN": 0.0, "MAX": 0.2 },

        { "NAME": "halationAmount", "TYPE": "float", "DEFAULT": 0.15, "MIN": 0.0, "MAX": 1.0 },
        { "NAME": "halationRadius", "TYPE": "float", "DEFAULT": 2.0, "MIN": 0.5, "MAX": 5.0 }
    ]
}*/

float luminance(vec3 c){
    return dot(c, vec3(0.2126, 0.7152, 0.0722));
}

// 時間対応ランダム
float rand(vec2 co, float t){
    return fract(sin(dot(co.xy ,vec2(12.9898,78.233)) + t) * 43758.5453);
}

// フィルムカーブ
vec3 filmCurve(vec3 col, float contrast, float fade){
    col = (col - 0.5) * contrast + 0.5;
    col = mix(col, vec3(luminance(col)), fade);
    return col;
}

// ハイライトロールオフ
vec3 highlightRollOff(vec3 col, float amt){
    float l = luminance(col);
    float roll = smoothstep(0.7, 1.0, l) * amt;
    return mix(col, vec3(l), roll * 0.5);
}

// Teal & Orange
vec3 tealOrangeGrade(vec3 col, float amt){
    float l = luminance(col);
    vec3 shadows = vec3(0.0, 0.6, 0.7);
    vec3 highs   = vec3(1.0, 0.5, 0.2);
    vec3 grade = mix(shadows, highs, l);
    return mix(col, col * grade, amt);
}

// 彩度
vec3 adjustSaturation(vec3 col, float sat){
    float l = luminance(col);
    return mix(vec3(l), col, sat);
}

// セピア
vec3 sepiaTone(vec3 col, float amt){
    vec3 sepia = vec3(
        dot(col, vec3(0.393, 0.769, 0.189)),
        dot(col, vec3(0.349, 0.686, 0.168)),
        dot(col, vec3(0.272, 0.534, 0.131))
    );
    return mix(col, sepia, amt);
}

// 色収差
vec3 chromaAberration(vec2 uv, float shift){
    float r = IMG_NORM_PIXEL(inputImage, uv + vec2(shift, 0.0)).r;
    float g = IMG_NORM_PIXEL(inputImage, uv).g;
    float b = IMG_NORM_PIXEL(inputImage, uv - vec2(shift, 0.0)).b;
    return vec3(r,g,b);
}

// ハレーション（赤にじみ）
vec3 halation(vec2 uv, float radius, float amt){
    vec3 sum = vec3(0.0);
    float count = 0.0;

    for(int x=-2; x<=2; x++){
        for(int y=-2; y<=2; y++){
            vec2 offset = vec2(x,y) * radius / RENDERSIZE.xy;
            vec3 c = IMG_NORM_PIXEL(inputImage, uv + offset).rgb;
            float l = luminance(c);
            float mask = smoothstep(0.6, 1.0, l);
            sum += vec3(c.r, 0.0, 0.0) * mask;
            count += 1.0;
        }
    }

    return (sum / count) * amt;
}

void main(){
    vec2 uv = isf_FragNormCoord;

    // 時間制御
    float t = TIME;
    if(posterizeFPS > 0.0){
        t = floor(TIME * posterizeFPS) / posterizeFPS;
    }

    // 色収差
    vec3 col = chromaAberration(uv, chromaShift);

    // フィルム処理
    col = filmCurve(col, contrast, fade);
    col = highlightRollOff(col, highlightRoll);
    col = tealOrangeGrade(col, tealOrange);
    col = adjustSaturation(col, saturation);

    // ガンマ
    col = pow(col, vec3(gamma));

    // フリッカー（12fps）
    float flick = rand(vec2(0.0), floor(t * 12.0));
    col *= 1.0 + (flick - 0.5) * flickerAmount;

    // ハレーション加算
    col += halation(uv, halationRadius, halationAmount);

    // グレイン
    float noise = rand(floor(uv * RENDERSIZE.xy * grainSize), floor(t * 24.0));
    col += (noise - 0.5) * grainAmount;

    // セピア
    col = sepiaTone(col, sepiaAmount);

    gl_FragColor = vec4(col, 1.0);
}