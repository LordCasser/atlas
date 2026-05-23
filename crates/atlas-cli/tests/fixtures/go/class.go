package main

type Greeter interface {
	Greet() string
}

type Person struct {
	Name string
}

func (p *Person) Greet() string {
	return "Hello, " + p.Name
}

func main() {
	var g Greeter = &Person{Name: "World"}
	_ = g.Greet()
}
