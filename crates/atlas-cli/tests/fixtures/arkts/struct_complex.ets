// Golden test fixture: ArkTS struct (complex)
// Covers: struct with decorator, fields, multiple methods, nested UI DSL
// Verifies that:
//  1. The struct symbol is recovered as kind=struct
//  2. The struct scope is recovered and covers the full declaration body
//  3. TS-parsed methods (build, increment, aboutToAppear) are bound to the struct
//  4. Decorator (@Component) and type annotations do not interfere with recovery

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
