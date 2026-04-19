# TID APC Injection
Bu proje, Windows işletim sisteminde hedef bir sürecin iş parçacıklarını (threads) kullanarak, kod yürütme (code execution) mekanizmalarını `QueueUserAPC` tekniği ile simüle etmeyi hedefler.

## Amaç ve Hedef
Projenin temel amacı, hedef süreçte yeni bir thread oluşturmak yerine, mevcut thread'lerin asenkron prosedür çağrısı (APC) kuyruğuna ekleme yaparak daha düşük profilli bir enjeksiyon gerçekleştirmektir. Yazılım, bellek yönetimini RAII prensipleriyle yaparak sistem kararlılığını korumayı hedefler.

## İşlem Akışı
1. **Süreç ve Thread Keşfi:** Hedef sürecin PID değeri ve bu sürece ait tüm aktif Thread ID (TID) listesi sistem snapshot'ı üzerinden alınır.
2. **Uzak Bellek Hazırlığı:** `VirtualAllocEx` ile hedef süreçte yer ayrılır ve payload `WriteProcessMemory` ile bu alana transfer edilir.
3. **Erişim Optimizasyonu:** Yazma işlemi bittikten sonra bellek izinleri `VirtualProtectEx` ile RX (Read/Execute) moduna çekilir.
4. **APC Kuyruklama:** Her bir aktif thread için `OpenThread` çağrısı yapılır ve `QueueUserAPC` ile kodun yürütülmesi emri kuyruğa iletilir.
5. **Bellek Kalıcılığı:** Kodun yürütülmesi tamamlanana kadar uzak belleğin serbest bırakılması (`Persistence`) manuel olarak kontrol edilir.

## Terminal Çıktı Analizi
* **APC Readiness:** `VirtualQueryEx` ile yapılan derin doğrulamanın sonucudur. Belleğin doğru durumda (Commit), doğru boyutta ve doğru başlangıç izinlerinde olduğunu teyit eder.
* **Memory protection optimized:** Bellek izinlerinin yazma modundan (RW), yürütme moduna (RX) başarıyla geçtiğini gösterir.
* **Successfully queued APC:** `QueueUserAPC` fonksiyonunun hedef thread'lere başarıyla ulaştığını ve kodun "uyarılabilir" (alertable) bir durum beklediğini ifade eder.
* **Persistence enabled:** RAII mekanizmasının belleği hemen silmemesi için devreye girdiğini, kodun uzak süreçte çalışabilmesi için alanın korunmaya alındığını gösterir.
* **Process handle closed:** İşlem bittiğinde `ProcessHandle` yapısının `Drop` trait'i üzerinden otomatik olarak kapatıldığını ve sistem kaynağının iade edildiğini temsil eder.

```bash
cargo run
