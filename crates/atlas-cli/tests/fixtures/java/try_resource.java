import java.io.*;

class ResourceTest {
    void readFile() throws IOException {
        try (FileInputStream fis = new FileInputStream("data.txt")) {
            byte[] buf = new byte[1024];
            fis.read(buf);
        }
    }
}
