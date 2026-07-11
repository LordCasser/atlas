// ArkTS core-pattern fixture: verifies fallback coverage, not confidence claims.
// - let/const block-scoped bindings (no var)
// - dot-only field access (no bracket access)
// - named function declarations (no function expressions)
// - as T casts (not <T> angle-bracket casts)
// - no destructuring anywhere

interface Model {
  id: number
  name: string
}

struct ConstraintDemo {
  private model: Model
  count: number

  constructor(model: Model) {
    this.model = model
    this.count = 0
  }

  increment(): void {
    this.count = this.count + 1
  }

  classify(value: number): string {
    let result: string
    if (value > 0) {
      result = "positive"
    } else {
      result = "non-positive"
    }
    return result
  }

  sumRange(start: number, end: number): number {
    let total: number = 0
    for (let i = start; i < end; i = i + 1) {
      total = total + i
    }
    return total
  }

  format(): string {
    let label: string = this.model.name as string
    return label
  }
}

function process(model: Model): number {
  let demo: ConstraintDemo = new ConstraintDemo(model)
  demo.increment()
  demo.increment()
  let classification: string = demo.classify(demo.count)
  let sum: number = demo.sumRange(0, 10)
  return sum
}

export { process, ConstraintDemo }
