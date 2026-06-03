import java.io.File

fun readFile() {
    val file = File("data.txt")
    file.bufferedReader().use { reader ->
        val line = reader.readLine()
        println(line)
    }
}
