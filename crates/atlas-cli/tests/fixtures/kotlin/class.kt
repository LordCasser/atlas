interface Greeter {
    fun greet(): String
}

class Person(val name: String) : Greeter {
    override fun greet(): String = "Hello, $name"
}

fun main() {
    val p = Person("World")
    println(p.greet())
}
