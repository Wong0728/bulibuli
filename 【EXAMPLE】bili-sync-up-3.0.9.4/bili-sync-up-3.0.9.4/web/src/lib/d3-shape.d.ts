declare module 'd3-shape' {
	export interface CurveFactory {
		(context: CanvasRenderingContext2D): unknown;
		lineStart(): void;
		lineEnd(): void;
		point(x: number, y: number): void;
	}

	export const curveNatural: CurveFactory;
	export const curveLinear: CurveFactory;
	export const curveBasis: CurveFactory;
	export const curveMonotoneX: CurveFactory;
	export const curveMonotoneY: CurveFactory;
	export const curveStep: CurveFactory;
	export const curveStepAfter: CurveFactory;
	export const curveStepBefore: CurveFactory;
}
