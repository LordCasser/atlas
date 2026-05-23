interface IGreeter {
    string Greet();
}

class Greeter : IGreeter {
    public string Name { get; set; }

    public string Greet() {
        return "Hello, " + Name;
    }

    static void Main() {
        var g = new Greeter { Name = "World" };
        g.Greet();
    }
}
