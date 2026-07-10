// Golden test fixture: ArkTS struct (complex)
// Covers: struct with decorator, fields, multiple methods, nested UI DSL
// Verifies that:
//  1. The struct symbol is normalized as kind=struct
//  2. Fields and real methods retain struct ownership
//  3. Nested UI DSL calls retain build() as their caller
//  4. Decorators and type annotations preserve original source ranges

@Component
struct ComplexWidget {
  @Prop title: string;
  @State count: number = 0;

  build() {
    Column() {
      Text(this.title)
    }
  }

  increment() {
    this.count += 1;
  }

  aboutToAppear(): void {
    console.info("ComplexWidget about to appear");
  }
}
